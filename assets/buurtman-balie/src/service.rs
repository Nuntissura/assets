use crate::*;

pub struct Crm<S, A> {
    store: S,
    authorization: A,
    limits: Limits,
}
impl<S: Store, A: Authorization> Crm<S, A> {
    pub fn new(store: S, authorization: A, limits: Limits) -> Self {
        Self {
            store,
            authorization,
            limits,
        }
    }
    fn authorize(&self, ctx: &Context, access: Access) -> Result<(), Error> {
        identifier(&ctx.workspace)?;
        identifier(&ctx.actor)?;
        if !self.authorization.allows(ctx, access) {
            return Err(Error::Denied);
        }
        Ok(())
    }
    fn load(&self, workspace: &str) -> Result<Snapshot, Error> {
        let snapshot = self.store.load(workspace)?;
        if snapshot.schema_version != 2 {
            return Err(Error::Invalid("snapshot schema"));
        }
        self.check_size(&snapshot)?;
        Ok(snapshot)
    }
    fn check_size(&self, snapshot: &Snapshot) -> Result<(), Error> {
        if snapshot.records.len() > self.limits.records
            || snapshot.relations.len() > self.limits.relations
            || snapshot.fields.len() > self.limits.fields
            || snapshot.receipts.len() > self.limits.operations
        {
            return Err(Error::Capacity("snapshot counts"));
        }
        let mut writer = BudgetWriter(self.limits.snapshot_bytes);
        serde_json::to_writer(&mut writer, snapshot).map_err(|_| Error::Capacity("snapshot bytes"))
    }
    pub fn get(&self, ctx: &Context, id: &str) -> Result<Record, Error> {
        self.authorize(ctx, Access::Read)?;
        identifier(id)?;
        self.load(&ctx.workspace)?
            .records
            .get(id)
            .cloned()
            .ok_or(Error::NotFound)
    }
    pub fn relations(&self, ctx: &Context, id: &str) -> Result<Vec<Relation>, Error> {
        self.get(ctx, id)?;
        Ok(self
            .load(&ctx.workspace)?
            .relations
            .into_iter()
            .filter(|r| {
                r.source == id || matches!(&r.target, Target::Record(target) if target == id)
            })
            .collect())
    }
    pub fn field_definitions(&self, ctx: &Context) -> Result<Vec<FieldDefinition>, Error> {
        self.authorize(ctx, Access::Read)?;
        Ok(self.load(&ctx.workspace)?.fields.into_values().collect())
    }
    pub fn pricing_tiers(&self, ctx: &Context) -> Result<Vec<PricingTier>, Error> {
        self.authorize(ctx, Access::Read)?;
        Ok(self
            .load(&ctx.workspace)?
            .pricing_tiers
            .into_values()
            .collect())
    }
    pub fn quote(&self, ctx: &Context, client_id: &str, product_id: &str) -> Result<Quote, Error> {
        self.authorize(ctx, Access::Read)?;
        identifier(client_id)?;
        identifier(product_id)?;
        crate::pricing::calculate(&self.load(&ctx.workspace)?, client_id, product_id)
    }
    /// No hidden retries. On conflict callers reload; uncertain outcomes retry the same request ID.
    pub fn execute(&mut self, ctx: &Context, request: Request) -> Result<Outcome, Error> {
        self.authorize(ctx, Access::Write)?;
        identifier(&request.operation_id)?;
        let mut state = self.load(&ctx.workspace)?;
        if let Some(receipt) = state.receipts.get(&request.operation_id) {
            return if receipt.actor == ctx.actor && receipt.request == request {
                Ok(receipt.outcome.clone())
            } else {
                Err(Error::OperationReused)
            };
        }
        if state.receipts.len() >= self.limits.operations {
            return Err(Error::Capacity("operations"));
        }
        let expected = state.revision;
        let record_revision = self.apply(&mut state, &request.command)?;
        state.revision = expected.checked_add(1).ok_or(Error::Capacity("revision"))?;
        let outcome = Outcome {
            workspace_revision: state.revision,
            record_revision,
        };
        state.receipts.insert(
            request.operation_id.clone(),
            Receipt {
                actor: ctx.actor.clone(),
                request,
                outcome: outcome.clone(),
            },
        );
        self.check_size(&state)?;
        self.store
            .compare_exchange(&ctx.workspace, expected, state)?;
        Ok(outcome)
    }
    fn apply(&self, state: &mut Snapshot, command: &Command) -> Result<Option<u64>, Error> {
        match command {
            Command::CreateLinked { record, relations } => {
                if relations.len() > 32 || relations.iter().any(|r| r.source != record.id) {
                    return Err(Error::Invalid("create links"));
                }
                let revision = self.apply(state, &Command::Create(record.clone()))?;
                for relation in relations {
                    self.apply(state, &Command::Link(relation.clone()))?;
                }
                Ok(revision)
            }
            Command::DefinePricingTier(tier) => {
                identifier(&tier.id)?;
                text(&tier.name, 128, false)?;
                if tier.discount_basis_points > 10000 {
                    return Err(Error::Invalid("discount range"));
                }
                if let PriceBasis::Custom(key) = &tier.base {
                    identifier(key)?;
                }
                if let Some(old) = state.pricing_tiers.get(&tier.id) {
                    if old != tier {
                        return Err(Error::Conflict);
                    }
                } else {
                    if state.pricing_tiers.len() >= 32 {
                        return Err(Error::Capacity("pricing tiers"));
                    }
                    state.pricing_tiers.insert(tier.id.clone(), tier.clone());
                }
                Ok(None)
            }
            Command::Create(input) => {
                validate_record(state, input)?;
                if input.revision != 0 || state.records.contains_key(&input.id) {
                    return Err(Error::Conflict);
                }
                if state.records.len() >= self.limits.records {
                    return Err(Error::Capacity("records"));
                }
                let mut record = input.clone();
                record.revision = 1;
                state.records.insert(record.id.clone(), record);
                Ok(Some(1))
            }
            Command::Update(input) => {
                validate_record(state, input)?;
                let old = state.records.get(&input.id).ok_or(Error::NotFound)?;
                if old.revision != input.revision || old.kind != input.kind {
                    return Err(Error::Conflict);
                }
                let revision = old
                    .revision
                    .checked_add(1)
                    .ok_or(Error::Capacity("revision"))?;
                let mut record = input.clone();
                record.revision = revision;
                state.records.insert(record.id.clone(), record);
                Ok(Some(revision))
            }
            Command::DefineField(definition) => {
                identifier(&definition.key)?;
                text(&definition.label, 128, false)?;
                if definition.applies_to.is_empty() || definition.applies_to.len() > 6 {
                    return Err(Error::Invalid("field applicability"));
                }
                for (i, kind) in definition.applies_to.iter().enumerate() {
                    if definition.applies_to[..i].contains(kind) {
                        return Err(Error::Invalid("duplicate field kind"));
                    }
                }
                if let Some(old) = state.fields.get(&definition.key) {
                    if old != definition {
                        return Err(Error::Conflict);
                    }
                } else {
                    if state.fields.len() >= self.limits.fields {
                        return Err(Error::Capacity("fields"));
                    }
                    state
                        .fields
                        .insert(definition.key.clone(), definition.clone());
                }
                Ok(None)
            }
            Command::Link(relation) | Command::Unlink(relation) => {
                identifier(&relation.source)?;
                text(&relation.label, 64, false)?;
                if !state.records.contains_key(&relation.source) {
                    return Err(Error::NotFound);
                }
                match &relation.target {
                    Target::Record(id) => {
                        identifier(id)?;
                        if id == &relation.source {
                            return Err(Error::Invalid("self relation"));
                        }
                        if !state.records.contains_key(id) {
                            return Err(Error::NotFound);
                        }
                    }
                    Target::External(reference) => {
                        identifier(&reference.owner)?;
                        identifier(&reference.resource_type)?;
                        identifier(&reference.id)?;
                    }
                }
                if matches!(command, Command::Unlink(_)) {
                    state.relations.retain(|r| r != relation);
                } else if !state.relations.contains(relation) {
                    if state.relations.len() >= self.limits.relations {
                        return Err(Error::Capacity("relations"));
                    }
                    state.relations.push(relation.clone());
                }
                Ok(None)
            }
        }
    }
    pub fn search(
        &self,
        ctx: &Context,
        query: &str,
        tag: Option<&str>,
        limit: usize,
        cursor: Option<&Cursor>,
    ) -> Result<Page, Error> {
        self.search_filtered(
            ctx,
            &SearchFilter {
                query: query.into(),
                tag: tag.map(str::to_owned),
                ..Default::default()
            },
            limit,
            cursor,
        )
    }
    pub fn search_filtered(
        &self,
        ctx: &Context,
        filter: &SearchFilter,
        limit: usize,
        cursor: Option<&Cursor>,
    ) -> Result<Page, Error> {
        let query = filter.query.as_str();
        let tag = filter.tag.as_deref();
        let fields = &filter.fields;
        let kind = filter.kind.as_ref();
        self.authorize(ctx, Access::Read)?;
        text(query, 64, true)?;
        if let Some(tag) = tag {
            text(tag, 64, false)?;
        }
        if !(1..=100).contains(&limit) {
            return Err(Error::Invalid("page limit"));
        }
        let state = self.load(&ctx.workspace)?;
        if fields.len() > self.limits.fields {
            return Err(Error::Capacity("search fields"));
        }
        for (key, value) in fields {
            let definition = state
                .fields
                .get(key)
                .ok_or(Error::Invalid("undefined filter field"))?;
            validate_value(&definition.value_type, value)?;
        }
        let query = query.trim().to_lowercase();
        let tag = tag.map(str::to_lowercase);
        if let Some(cursor) = cursor {
            if cursor.workspace != ctx.workspace
                || cursor.query != query
                || cursor.tag != tag
                || cursor.fields != *fields
                || cursor.kind.as_ref() != kind
                || cursor.revision != state.revision
            {
                return Err(Error::Conflict);
            }
        }
        let mut hits: Vec<_> = state
            .records
            .values()
            .filter_map(|record| {
                if kind.is_some_and(|kind| *kind != record.kind) {
                    return None;
                }
                if fields
                    .iter()
                    .any(|(key, value)| record.fields.get(key) != Some(value))
                {
                    return None;
                }
                if tag
                    .as_ref()
                    .is_some_and(|tag| !record.tags.iter().any(|t| t.to_lowercase() == *tag))
                {
                    return None;
                }
                let name = record.name.to_lowercase();
                let distance = if query.is_empty()
                    || name.contains(&query)
                    || record
                        .tags
                        .iter()
                        .any(|t| t.to_lowercase().contains(&query))
                {
                    0
                } else {
                    std::iter::once(name.as_str())
                        .chain(name.split_whitespace())
                        .map(|word| edit_distance(&query, word))
                        .min()
                        .unwrap_or(usize::MAX)
                };
                (distance <= 2).then(|| SearchHit {
                    record: record.clone(),
                    distance,
                })
            })
            .collect();
        hits.sort_by(|a, b| (a.distance, &a.record.id).cmp(&(b.distance, &b.record.id)));
        let total = hits.len();
        if let Some(cursor) = cursor {
            hits.retain(|h| (h.distance, &h.record.id) > (cursor.distance, &cursor.id));
        }
        let more = hits.len() > limit;
        hits.truncate(limit);
        let next = if more {
            hits.last().map(|h| Cursor {
                workspace: ctx.workspace.clone(),
                query,
                tag,
                fields: fields.clone(),
                kind: kind.cloned(),
                revision: state.revision,
                distance: h.distance,
                id: h.record.id.clone(),
            })
        } else {
            None
        };
        Ok(Page {
            hits,
            next,
            total,
            revision: state.revision,
        })
    }
}

struct BudgetWriter(usize);
impl std::io::Write for BudgetWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > self.0 {
            return Err(std::io::Error::other("snapshot budget"));
        }
        self.0 -= bytes.len();
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn identifier(value: &str) -> Result<(), Error> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"-_.:".contains(&c))
    {
        return Err(Error::Invalid("identifier"));
    }
    Ok(())
}
fn text(value: &str, max: usize, empty: bool) -> Result<(), Error> {
    if (!empty && value.trim().is_empty())
        || value.chars().count() > max
        || value
            .chars()
            .any(|c| c.is_control() && c != '\n' && c != '\t')
    {
        return Err(Error::Invalid("text"));
    }
    Ok(())
}
fn validate_record(state: &Snapshot, record: &Record) -> Result<(), Error> {
    identifier(&record.id)?;
    text(&record.name, 128, false)?;
    text(&record.description, 8192, true)?;
    if (record.kind == Kind::Customer) != record.client_terms.is_some() {
        return Err(Error::Invalid(
            "client classification required for customers only",
        ));
    }
    if let Some(terms) = &record.client_terms {
        if let ClientPricing::Tier(id) = &terms.pricing {
            if !state.pricing_tiers.contains_key(id) {
                return Err(Error::Invalid("unknown pricing tier"));
            }
        }
    }
    if let Some(book) = &record.prices {
        if record.kind != Kind::Product {
            return Err(Error::Invalid("prices belong to products"));
        }
        crate::pricing::validate_book(book)?;
        for key in book.custom.keys() {
            identifier(key)?;
        }
    }
    if (record.kind == Kind::SupportCase) != record.case_status.is_some() {
        return Err(Error::Invalid("case status"));
    }
    if record.tags.len() > 32 || record.fields.len() > 128 {
        return Err(Error::Capacity("record fields/tags"));
    }
    for tag in &record.tags {
        text(tag, 64, false)?;
    }
    for (key, value) in &record.fields {
        let definition = state
            .fields
            .get(key)
            .ok_or(Error::Invalid("undefined field"))?;
        if !definition.applies_to.contains(&record.kind) {
            return Err(Error::Invalid("field record kind"));
        }
        validate_value(&definition.value_type, value)?;
    }
    Ok(())
}
fn edit_distance(a: &str, b: &str) -> usize {
    let b: Vec<_> = b.chars().collect();
    let mut row: Vec<_> = (0..=b.len()).collect();
    for (i, ac) in a.chars().enumerate() {
        let mut previous = row[0];
        row[0] = i + 1;
        for (j, bc) in b.iter().enumerate() {
            let old = row[j + 1];
            row[j + 1] = (row[j] + 1)
                .min(old + 1)
                .min(previous + usize::from(ac != *bc));
            previous = old;
        }
    }
    row[b.len()]
}

fn validate_value(value_type: &FieldType, value: &Value) -> Result<(), Error> {
    match (value_type, value) {
        (FieldType::Text, Value::Text(text_value)) => text(text_value, 2048, true),
        (FieldType::Integer, Value::Integer(_)) | (FieldType::Boolean, Value::Boolean(_)) => Ok(()),
        _ => Err(Error::Invalid("field type")),
    }
}
