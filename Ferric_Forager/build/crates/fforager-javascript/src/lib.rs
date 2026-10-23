//! Non-shipped Ferric-owned `YouTube` JavaScript challenge prototype.
//!
//! Raw player programs are parsed and reduced before evaluation. Every request
//! receives a new Boa context with no registered host capabilities.

#![forbid(unsafe_code)]

use boa_ast::{
    Expression, Spanned, Statement, StatementListItem,
    declaration::Binding,
    expression::access::PropertyAccess,
    expression::operator::assign::AssignOp,
    function::FunctionExpression,
    visitor::{VisitWith, Visitor},
};
use boa_engine::{Context, Source, context::ContextBuilder, vm::RuntimeLimits};
use boa_interner::{ToIndentedString, ToInternedString};
use boa_parser::Parser;
use fforager_contracts::{FrameError, FrameLimits};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    fmt,
    fmt::Write as _,
    io::{self, Read, Write},
    ops::ControlFlow,
    time::Instant,
};

pub const ENGINE_ID: &str = "boa_engine@0.21.1";
pub const SOLVER_IMPLEMENTATION: &str = "ferric-player-ast-v1";
pub const MAXIMUM_FRAME_BYTES: usize = 1024 * 1024;
pub const MAXIMUM_PROGRAM_BYTES: usize = 4 * 1024 * 1024;
pub const MAXIMUM_OUTPUT_BYTES: usize = 1024 * 1024;
pub const MAXIMUM_LOOP_ITERATIONS: u64 = 5_000_000;
pub const MAXIMUM_RECURSION_DEPTH: usize = 512;
pub const MAXIMUM_STACK_SIZE: usize = 10_240;
pub const MAXIMUM_NATIVE_THREAD_STACK_BYTES: usize = 64 * 1024 * 1024;
pub const MAXIMUM_INSTRUCTIONS: usize = 50_000_000;
pub const MAXIMUM_JOBS_PER_WORKER: u64 = 64;
pub const MAXIMUM_WORKER_AGE_MILLIS: u64 = 300_000;
pub const MAXIMUM_CACHE_ENTRIES: usize = 8;
pub const MAXIMUM_CACHE_BYTES: usize = 24 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChallengeKind {
    N,
    Signature,
    CapabilityProbe,
    StateSeed,
    StateCheck,
    InfiniteLoop,
    MemoryBomb,
    Crash,
}

impl ChallengeKind {
    #[must_use]
    pub const fn execution_mode(self) -> &'static str {
        match self {
            Self::N => "n",
            Self::Signature => "signature",
            Self::CapabilityProbe => "capability_probe",
            Self::StateSeed => "state_seed",
            Self::StateCheck => "state_check",
            Self::InfiniteLoop => "infinite_loop",
            Self::MemoryBomb => "memory_bomb",
            Self::Crash => "crash",
        }
    }

    #[must_use]
    pub const fn requires_player(self) -> bool {
        matches!(self, Self::N | Self::Signature)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerInput {
    pub challenge: ChallengeKind,
    pub value: String,
    pub script_sha256: String,
    pub extractor_version: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerOutput {
    pub engine: String,
    pub solver_implementation: String,
    pub challenge: ChallengeKind,
    pub script_sha256: String,
    pub prepared_sha256: String,
    pub candidate_count: usize,
    pub successful_candidates: usize,
    pub result: String,
    pub duration_millis: u64,
    pub cache_hit: bool,
    pub fresh_context: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct CacheKey {
    pub script_sha256: String,
    pub solver_implementation: String,
    pub engine: String,
    pub execution_mode: String,
    pub extractor_version: String,
}

impl CacheKey {
    #[must_use]
    pub fn from_input(input: &WorkerInput) -> Self {
        Self {
            script_sha256: input.script_sha256.clone(),
            solver_implementation: SOLVER_IMPLEMENTATION.to_owned(),
            engine: ENGINE_ID.to_owned(),
            execution_mode: input.challenge.execution_mode().to_owned(),
            extractor_version: input.extractor_version.clone(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreparedProgram {
    pub source: String,
    pub source_sha256: String,
    pub candidate_count: usize,
}

#[derive(Clone, Debug)]
pub struct WorkerCache {
    entries: BTreeMap<CacheKey, PreparedProgram>,
    insertion_order: VecDeque<CacheKey>,
    total_bytes: usize,
    maximum_entries: usize,
    maximum_bytes: usize,
}

impl Default for WorkerCache {
    fn default() -> Self {
        Self::new(MAXIMUM_CACHE_ENTRIES, MAXIMUM_CACHE_BYTES)
    }
}

impl WorkerCache {
    #[must_use]
    pub fn new(maximum_entries: usize, maximum_bytes: usize) -> Self {
        Self {
            entries: BTreeMap::new(),
            insertion_order: VecDeque::new(),
            total_bytes: 0,
            maximum_entries,
            maximum_bytes,
        }
    }

    #[must_use]
    pub fn get(&self, key: &CacheKey) -> Option<&PreparedProgram> {
        self.entries.get(key)
    }

    pub fn insert(&mut self, key: CacheKey, program: PreparedProgram) {
        let program_bytes = program.source.len();
        if program_bytes > self.maximum_bytes || self.maximum_entries == 0 {
            return;
        }
        if let Some(previous) = self.entries.remove(&key) {
            self.total_bytes = self.total_bytes.saturating_sub(previous.source.len());
            self.insertion_order.retain(|existing| existing != &key);
        }
        while self.entries.len() >= self.maximum_entries
            || self.total_bytes.saturating_add(program_bytes) > self.maximum_bytes
        {
            let Some(oldest) = self.insertion_order.pop_front() else {
                break;
            };
            if let Some(removed) = self.entries.remove(&oldest) {
                self.total_bytes = self.total_bytes.saturating_sub(removed.source.len());
            }
        }
        self.total_bytes = self.total_bytes.saturating_add(program_bytes);
        self.insertion_order.push_back(key.clone());
        self.entries.insert(key, program);
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    #[must_use]
    pub const fn total_bytes(&self) -> usize {
        self.total_bytes
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SolverError {
    ProgramTooLarge { actual: usize, maximum: usize },
    InvalidProgram(String),
    UnsupportedPlayerStructure,
    NoChallengeCandidate,
    InvalidCandidate(String),
    ScriptHashMismatch { expected: String, observed: String },
    Evaluation(String),
    InvalidResult(String),
    OutputTooLarge { actual: usize, maximum: usize },
    Io(String),
    Frame(String),
}

impl fmt::Display for SolverError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{self:?}")
    }
}

impl std::error::Error for SolverError {}

#[must_use]
pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut encoded = String::with_capacity(digest.len() * 2);
    for byte in digest {
        write!(&mut encoded, "{byte:02x}").expect("writing to a String cannot fail");
    }
    encoded
}

/// Verifies the exact SHA-256 identity of a raw player program.
///
/// # Errors
///
/// Returns [`SolverError::ScriptHashMismatch`] when the observed digest differs.
pub fn verify_program_hash(program: &[u8], expected: &str) -> Result<String, SolverError> {
    let observed = sha256_hex(program);
    if observed != expected {
        return Err(SolverError::ScriptHashMismatch {
            expected: expected.to_owned(),
            observed,
        });
    }
    Ok(expected.to_owned())
}

/// Discovers challenge candidates and builds the Ferric-owned reduced program.
///
/// # Errors
///
/// Returns a bounded parse, structure, or candidate error when the raw player
/// cannot be safely reduced.
#[allow(clippy::too_many_lines)]
pub fn prepare_player(program: &[u8]) -> Result<PreparedProgram, SolverError> {
    if program.len() > MAXIMUM_PROGRAM_BYTES {
        return Err(SolverError::ProgramTooLarge {
            actual: program.len(),
            maximum: MAXIMUM_PROGRAM_BYTES,
        });
    }
    if !program.is_ascii() {
        return Err(SolverError::InvalidProgram(
            "raw player must be ASCII so AST source spans remain byte-exact".to_owned(),
        ));
    }
    let program_source = std::str::from_utf8(program)
        .map_err(|error| SolverError::InvalidProgram(error.to_string()))?;
    let line_offsets = line_offsets(program);
    let mut parse_context = Context::default();
    let scope = parse_context.realm().scope().clone();
    let mut parser = Parser::new(Source::from_bytes(program));
    let script = parser
        .parse_script(&scope, parse_context.interner_mut())
        .map_err(|error| SolverError::InvalidProgram(error.to_string()))?;
    let interner = parse_context.interner();
    let mut selected = None;
    for outer in outer_iifes(&script) {
        let mut discovered = BTreeSet::new();
        for item in outer.body().statements() {
            for candidate in candidate_expressions(item, interner) {
                if !valid_candidate_expression(&candidate) {
                    return Err(SolverError::InvalidCandidate(candidate));
                }
                discovered.insert(candidate);
            }
        }
        if !discovered.is_empty() {
            selected = Some((outer, discovered));
            break;
        }
    }
    let (outer, candidates) = selected.ok_or(SolverError::NoChallengeCandidate)?;
    let parameters = outer
        .parameters()
        .as_ref()
        .iter()
        .map(|parameter| parameter.to_interned_string(interner))
        .collect::<Vec<_>>()
        .join(", ");
    let hoisted_vars = collect_outer_var_names(outer, interner);
    let mut retained = String::new();

    for item in outer.body().statements() {
        if !retain_statement(item, interner) {
            continue;
        }
        let rendered = item.to_indented_string(interner, 1);
        if rendered.trim() == "var window = this;" {
            continue;
        }
        if let Some(raw) = raw_retained_statement(program_source, &line_offsets, item)? {
            let guarded_setup_statement = matches!(
                item,
                StatementListItem::Statement(statement)
                    if matches!(statement.as_ref(), Statement::Var(_))
                        || matches!(
                            statement.as_ref(),
                            Statement::Expression(Expression::Assign(_))
                        )
            );
            if guarded_setup_statement {
                retained.push_str("try{");
                retained.push_str(raw);
                retained.push_str("}catch(__ff_definition_error){}");
            } else {
                retained.push_str(raw);
            }
        } else {
            retained.push_str(&rendered);
        }
        retained.push('\n');
    }
    if candidates.is_empty() {
        return Err(SolverError::NoChallengeCandidate);
    }

    let candidate_list = candidates
        .iter()
        .map(|candidate| {
            let name = serde_json::to_string(candidate)
                .unwrap_or_else(|_| "\"invalid-candidate-name\"".to_owned());
            format!("{{name:{name},run:{candidate}}}")
        })
        .collect::<Vec<_>>()
        .join(", ");
    let source = format!(
        r#"
globalThis.XMLHttpRequest = {{ prototype: {{}} }};
globalThis.location = {{
    hash: "", host: "www.youtube.com", hostname: "www.youtube.com",
    href: "https://www.youtube.com/watch?v=ferric", origin: "https://www.youtube.com",
    password: "", pathname: "/watch", port: "", protocol: "https:",
    search: "?v=ferric", username: ""
}};
globalThis.document = Object.create(null);
globalThis.navigator = Object.create(null);
globalThis.self = globalThis;
globalThis.window = globalThis;
class __FerricUrl {{
    constructor(value) {{
        this._base = String(value).split("?")[0];
        this._params = Object.create(null);
        const query = String(value).split("?")[1] || "";
        for (const entry of query.split("&")) {{
            if (!entry) continue;
            const pair = entry.split("=");
            this.set(decodeURIComponent(pair.shift()), decodeURIComponent(pair.join("=")));
        }}
    }}
    set(key, value) {{ this._params[String(key)] = String(value); return this; }}
    get(key) {{
        key = String(key);
        return Object.prototype.hasOwnProperty.call(this._params, key)
            ? this._params[key]
            : null;
    }}
    clone() {{
        const copy = new __FerricUrl(this._base);
        for (const key of Object.keys(this._params)) copy.set(key, this._params[key]);
        return copy;
    }}
    Xl() {{ return this.toString(); }}
    toString() {{
        const query = Object.keys(this._params)
            .map(key => encodeURIComponent(key) + "=" + encodeURIComponent(this._params[key]))
            .join("&");
        return this._base + (query ? "?" + query : "");
    }}
}}
const __ff_scope = Object.create(null);
(function({parameters}) {{
var {hoisted_vars};
{retained}
    if (typeof __ff_scope.sB !== "function") __ff_scope.sB = __FerricUrl;
    const __ff_candidates = [{candidate_list}];
    globalThis.__ff_solve = function(kind, input) {{
        const results = [];
        const errors = [];
        for (const candidate of __ff_candidates) {{
            try {{
                const url = candidate.run(
                    "https://www.youtube.com/watch?v=ferric",
                    "s",
                    kind === "signature" ? encodeURIComponent(input) : undefined
                );
                let value;
                if (kind === "signature") {{
                    value = decodeURIComponent(url.get("s"));
                }} else {{
                    url.set("n", input);
                    const proto = Object.getPrototypeOf(url);
                    const keys = Object.keys(proto).concat(Object.getOwnPropertyNames(proto));
                    let invoked = false;
                    for (const method of keys) {{
                        if (!["constructor", "set", "get", "clone"].includes(method)
                            && typeof url[method] === "function") {{
                            url[method]();
                            invoked = true;
                            break;
                        }}
                    }}
                    if (!invoked) throw new Error("no n-transform prototype method");
                    value = url.get("n");
                }}
                if (typeof value !== "string") throw new Error("challenge result is not a string");
                results.push(value);
            }} catch (error) {{
                errors.push(candidate.name + ": " + String(error)
                    + (error && error.stack ? " | " + error.stack : ""));
            }}
        }}
        if (results.length === 0) throw new Error("no candidate succeeded: " + errors.join(" | "));
        const unique = Array.from(new Set(results));
        if (unique.length !== 1) throw new Error("candidate disagreement: " + JSON.stringify(unique));
        return {{
            value: unique[0],
            successful_candidates: results.length
        }};
    }};
}})(__ff_scope);
"#,
        hoisted_vars = hoisted_vars.into_iter().collect::<Vec<_>>().join(",")
    );
    let source_sha256 = sha256_hex(source.as_bytes());
    Ok(PreparedProgram {
        source,
        source_sha256,
        candidate_count: candidates.len(),
    })
}

struct OuterVarCollector<'a> {
    interner: &'a boa_interner::Interner,
    names: BTreeSet<String>,
}

impl<'ast> Visitor<'ast> for OuterVarCollector<'_> {
    type BreakTy = ();

    fn visit_var_declaration(
        &mut self,
        declaration: &'ast boa_ast::declaration::VarDeclaration,
    ) -> ControlFlow<Self::BreakTy> {
        for variable in declaration.0.as_ref() {
            if let boa_ast::declaration::Binding::Identifier(identifier) = variable.binding() {
                self.names
                    .insert(self.interner.resolve_expect(identifier.sym()).to_string());
            }
        }
        ControlFlow::Continue(())
    }

    fn visit_function_expression(
        &mut self,
        _function: &'ast FunctionExpression,
    ) -> ControlFlow<Self::BreakTy> {
        ControlFlow::Continue(())
    }

    fn visit_arrow_function(
        &mut self,
        _function: &'ast boa_ast::function::ArrowFunction,
    ) -> ControlFlow<Self::BreakTy> {
        ControlFlow::Continue(())
    }

    fn visit_async_arrow_function(
        &mut self,
        _function: &'ast boa_ast::function::AsyncArrowFunction,
    ) -> ControlFlow<Self::BreakTy> {
        ControlFlow::Continue(())
    }

    fn visit_generator_expression(
        &mut self,
        _function: &'ast boa_ast::function::GeneratorExpression,
    ) -> ControlFlow<Self::BreakTy> {
        ControlFlow::Continue(())
    }

    fn visit_async_function_expression(
        &mut self,
        _function: &'ast boa_ast::function::AsyncFunctionExpression,
    ) -> ControlFlow<Self::BreakTy> {
        ControlFlow::Continue(())
    }

    fn visit_async_generator_expression(
        &mut self,
        _function: &'ast boa_ast::function::AsyncGeneratorExpression,
    ) -> ControlFlow<Self::BreakTy> {
        ControlFlow::Continue(())
    }
}

fn collect_outer_var_names(
    outer: &FunctionExpression,
    interner: &boa_interner::Interner,
) -> BTreeSet<String> {
    let mut collector = OuterVarCollector {
        interner,
        names: BTreeSet::new(),
    };
    let _ = outer.body().visit_with(&mut collector);
    collector.names
}

fn raw_retained_statement<'a>(
    source: &'a str,
    offsets: &[usize],
    item: &StatementListItem,
) -> Result<Option<&'a str>, SolverError> {
    let StatementListItem::Statement(statement) = item else {
        return Ok(None);
    };
    match statement.as_ref() {
        Statement::Expression(expression) => {
            source_for_span(source, offsets, expression.span()).map(Some)
        }
        Statement::Var(declaration) => {
            let variables = declaration.0.as_ref();
            let Some(first) = variables.first() else {
                return Ok(None);
            };
            let Some(last) = variables.last() else {
                return Ok(None);
            };
            let boa_ast::declaration::Binding::Identifier(first_identifier) = first.binding()
            else {
                return Ok(None);
            };
            let first_binding = source_offset(offsets, first_identifier.span().start())?;
            let keyword = source[..first_binding]
                .rfind("var")
                .filter(|start| {
                    source[start.saturating_add(3)..first_binding]
                        .bytes()
                        .all(|byte| byte.is_ascii_whitespace())
                })
                .ok_or_else(|| {
                    SolverError::InvalidProgram(
                        "could not recover raw var declaration start".to_owned(),
                    )
                })?;
            let end_span = if let Some(initializer) = last.init() {
                initializer.span()
            } else if let boa_ast::declaration::Binding::Identifier(identifier) = last.binding() {
                identifier.span()
            } else {
                return Ok(None);
            };
            let end = source_offset(offsets, end_span.end())?;
            source.get(keyword..end).map(Some).ok_or_else(|| {
                SolverError::InvalidProgram("raw var declaration escaped player bytes".to_owned())
            })
        }
        _ => Ok(None),
    }
}

fn line_offsets(source: &[u8]) -> Vec<usize> {
    let mut offsets = vec![0];
    for (index, byte) in source.iter().enumerate() {
        if *byte == b'\n' {
            offsets.push(index.saturating_add(1));
        }
    }
    offsets
}

fn source_for_span<'a>(
    source: &'a str,
    offsets: &[usize],
    span: boa_ast::Span,
) -> Result<&'a str, SolverError> {
    let start = source_offset(offsets, span.start())?;
    let end = source_offset(offsets, span.end())?;
    source
        .get(start..end)
        .ok_or_else(|| SolverError::InvalidProgram("AST span escaped raw player bytes".to_owned()))
}

fn source_offset(offsets: &[usize], position: boa_ast::Position) -> Result<usize, SolverError> {
    let line = usize::try_from(position.line_number())
        .map_err(|_| SolverError::InvalidProgram("line number overflow".to_owned()))?;
    let column = usize::try_from(position.column_number())
        .map_err(|_| SolverError::InvalidProgram("column number overflow".to_owned()))?;
    offsets
        .get(line.saturating_sub(1))
        .and_then(|offset| offset.checked_add(column.saturating_sub(1)))
        .ok_or_else(|| SolverError::InvalidProgram("AST position escaped raw player".to_owned()))
}

fn outer_iifes(script: &boa_ast::Script) -> Vec<&FunctionExpression> {
    script
        .statements()
        .statements()
        .iter()
        .filter_map(|item| {
            let StatementListItem::Statement(statement) = item else {
                return None;
            };
            let Statement::Expression(Expression::Call(call)) = statement.as_ref() else {
                return None;
            };
            function_from_callee(call.function())
        })
        .collect()
}

fn function_from_callee(expression: &Expression) -> Option<&FunctionExpression> {
    match expression.flatten() {
        Expression::FunctionExpression(function) => Some(function),
        Expression::PropertyAccess(PropertyAccess::Simple(access)) => {
            match access.target().flatten() {
                Expression::FunctionExpression(function) => Some(function),
                _ => None,
            }
        }
        _ => None,
    }
}

fn retain_statement(item: &StatementListItem, _interner: &boa_interner::Interner) -> bool {
    match item {
        StatementListItem::Declaration(_) => false,
        StatementListItem::Statement(statement) => match statement.as_ref() {
            Statement::Var(_) => true,
            Statement::Expression(expression) => {
                matches!(expression, Expression::Assign(_) | Expression::Literal(_))
            }
            _ => false,
        },
    }
}

struct ChallengeShapeVisitor<'a> {
    interner: &'a boa_interner::Interner,
    saw_alr: bool,
    saw_yes: bool,
    saw_function: bool,
}

impl<'ast> Visitor<'ast> for ChallengeShapeVisitor<'_> {
    type BreakTy = ();

    fn visit_expression(&mut self, expression: &'ast Expression) -> ControlFlow<Self::BreakTy> {
        match expression {
            Expression::FunctionExpression(_) => self.saw_function = true,
            Expression::Literal(literal) => {
                if let Some(symbol) = literal.as_string() {
                    let value = self.interner.resolve_expect(symbol).to_string();
                    match value.as_str() {
                        "alr" => self.saw_alr = true,
                        "yes" => self.saw_yes = true,
                        _ => {}
                    }
                }
            }
            _ => {}
        }
        expression.visit_with(self)
    }
}

fn expression_has_challenge_shape(
    expression: &Expression,
    interner: &boa_interner::Interner,
) -> bool {
    let mut visitor = ChallengeShapeVisitor {
        interner,
        saw_alr: false,
        saw_yes: false,
        saw_function: matches!(expression.flatten(), Expression::FunctionExpression(_)),
    };
    let _ = expression.visit_with(&mut visitor);
    visitor.saw_alr && visitor.saw_yes && visitor.saw_function
}

fn candidate_expressions(
    item: &StatementListItem,
    interner: &boa_interner::Interner,
) -> Vec<String> {
    let mut candidates = Vec::new();
    let StatementListItem::Statement(statement) = item else {
        return candidates;
    };
    match statement.as_ref() {
        Statement::Expression(Expression::Assign(assign))
            if assign.op() == AssignOp::Assign
                && matches!(assign.rhs().flatten(), Expression::FunctionExpression(_))
                && expression_has_challenge_shape(assign.rhs(), interner) =>
        {
            candidates.push(assign.lhs().to_interned_string(interner));
        }
        Statement::Var(declaration) => {
            for variable in declaration.0.as_ref() {
                let Some(initializer) = variable.init() else {
                    continue;
                };
                if !matches!(initializer.flatten(), Expression::FunctionExpression(_))
                    || !expression_has_challenge_shape(initializer, interner)
                {
                    continue;
                }
                if let Binding::Identifier(identifier) = variable.binding() {
                    candidates.push(identifier.to_interned_string(interner));
                }
            }
        }
        _ => {}
    }
    candidates
}

fn is_candidate_character(character: char) -> bool {
    character.is_ascii_alphanumeric()
        || matches!(
            character,
            '_' | '$' | '.' | '[' | ']' | '"' | '\'' | '0'..='9'
        )
}

fn valid_candidate_expression(candidate: &str) -> bool {
    !candidate.is_empty()
        && candidate.len() <= 128
        && candidate.chars().all(is_candidate_character)
        && !candidate.contains("..")
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EvaluationResult {
    value: String,
    successful_candidates: usize,
}

/// Executes one challenge against a previously prepared player in a fresh context.
///
/// # Errors
///
/// Returns a bounded evaluation, candidate, or output error.
pub fn execute_prepared(
    prepared: &PreparedProgram,
    input: &WorkerInput,
    cache_hit: bool,
) -> Result<WorkerOutput, SolverError> {
    let started = Instant::now();
    let mut context = bounded_context(MAXIMUM_INSTRUCTIONS, MAXIMUM_LOOP_ITERATIONS)?;
    context
        .eval(Source::from_bytes(prepared.source.as_bytes()))
        .map_err(|error| {
            SolverError::Evaluation(evaluation_diagnostic(&prepared.source, &error.to_string()))
        })?;
    let challenge = serde_json::to_string(input.challenge.execution_mode())
        .map_err(|error| SolverError::InvalidResult(error.to_string()))?;
    let value = serde_json::to_string(&input.value)
        .map_err(|error| SolverError::InvalidResult(error.to_string()))?;
    let invocation = format!("JSON.stringify(globalThis.__ff_solve({challenge}, {value}))");
    let evaluated = context
        .eval(Source::from_bytes(invocation.as_bytes()))
        .map_err(|error| SolverError::Evaluation(error.to_string()))?;
    let json = evaluated
        .to_string(&mut context)
        .map_err(|error| SolverError::Evaluation(error.to_string()))?
        .to_std_string_escaped();
    if json.len() > MAXIMUM_OUTPUT_BYTES {
        return Err(SolverError::OutputTooLarge {
            actual: json.len(),
            maximum: MAXIMUM_OUTPUT_BYTES,
        });
    }
    let result: EvaluationResult = serde_json::from_str(&json)
        .map_err(|error| SolverError::InvalidResult(error.to_string()))?;
    drop(context);
    boa_gc::force_collect();
    Ok(WorkerOutput {
        engine: ENGINE_ID.to_owned(),
        solver_implementation: SOLVER_IMPLEMENTATION.to_owned(),
        challenge: input.challenge,
        script_sha256: input.script_sha256.clone(),
        prepared_sha256: prepared.source_sha256.clone(),
        candidate_count: prepared.candidate_count,
        successful_candidates: result.successful_candidates,
        result: result.value,
        duration_millis: millis_u64(started.elapsed().as_millis()),
        cache_hit,
        fresh_context: true,
    })
}

/// Executes an internal isolation/resource probe in a fresh bounded context.
///
/// # Errors
///
/// Returns a bounded evaluation or output error.
pub fn execute_probe(input: &WorkerInput) -> Result<WorkerOutput, SolverError> {
    let started = Instant::now();
    let (source, instructions, loops) = match input.challenge {
        ChallengeKind::CapabilityProbe => (
            r"JSON.stringify({
                fetch: typeof fetch,
                require: typeof require,
                process: typeof process,
                deno: typeof Deno,
                bun: typeof Bun,
                console: typeof console
            })",
            MAXIMUM_INSTRUCTIONS,
            MAXIMUM_LOOP_ITERATIONS,
        ),
        ChallengeKind::StateSeed => (
            r#"globalThis.__ff_cross_job_secret = "forbidden"; "seeded""#,
            MAXIMUM_INSTRUCTIONS,
            MAXIMUM_LOOP_ITERATIONS,
        ),
        ChallengeKind::StateCheck => (
            r"typeof globalThis.__ff_cross_job_secret",
            MAXIMUM_INSTRUCTIONS,
            MAXIMUM_LOOP_ITERATIONS,
        ),
        ChallengeKind::InfiniteLoop => (
            r"for (;;) { globalThis.__ff_spin = 1; }",
            usize::MAX,
            u64::MAX,
        ),
        ChallengeKind::MemoryBomb => (
            r#"const a = []; for (;;) { a.push("x".repeat(65536)); }"#,
            usize::MAX,
            u64::MAX,
        ),
        ChallengeKind::Crash => {
            std::process::exit(97);
        }
        ChallengeKind::N | ChallengeKind::Signature => {
            return Err(SolverError::InvalidResult(
                "player challenge routed to probe evaluator".to_owned(),
            ));
        }
    };
    let mut context = bounded_context(instructions, loops)?;
    let evaluated = context
        .eval(Source::from_bytes(source.as_bytes()))
        .map_err(|error| SolverError::Evaluation(error.to_string()))?;
    let result = evaluated
        .to_string(&mut context)
        .map_err(|error| SolverError::Evaluation(error.to_string()))?
        .to_std_string_escaped();
    Ok(WorkerOutput {
        engine: ENGINE_ID.to_owned(),
        solver_implementation: SOLVER_IMPLEMENTATION.to_owned(),
        challenge: input.challenge,
        script_sha256: input.script_sha256.clone(),
        prepared_sha256: sha256_hex(source.as_bytes()),
        candidate_count: 0,
        successful_candidates: 1,
        result,
        duration_millis: millis_u64(started.elapsed().as_millis()),
        cache_hit: false,
        fresh_context: true,
    })
}

fn bounded_context(instructions: usize, loops: u64) -> Result<Context, SolverError> {
    let mut context = ContextBuilder::new()
        .instructions_remaining(instructions)
        .build()
        .map_err(|error| SolverError::Evaluation(error.to_string()))?;
    let mut runtime_limits = RuntimeLimits::default();
    runtime_limits.set_loop_iteration_limit(loops);
    runtime_limits.set_recursion_limit(MAXIMUM_RECURSION_DEPTH);
    runtime_limits.set_stack_size_limit(MAXIMUM_STACK_SIZE);
    context.set_runtime_limits(runtime_limits);
    Ok(context)
}

fn millis_u64(value: u128) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

fn evaluation_diagnostic(source: &str, error: &str) -> String {
    let line = error
        .split("line ")
        .nth(1)
        .and_then(|suffix| {
            suffix
                .split(|character: char| !character.is_ascii_digit())
                .next()
        })
        .and_then(|digits| digits.parse::<usize>().ok())
        .or_else(|| {
            error
                .split("unknown at :")
                .nth(1)
                .and_then(|suffix| {
                    suffix
                        .split(|character: char| !character.is_ascii_digit())
                        .next()
                })
                .and_then(|digits| digits.parse::<usize>().ok())
        });
    let Some(line) = line else {
        return error.to_owned();
    };
    let start = line.saturating_sub(3);
    let excerpt = source
        .lines()
        .enumerate()
        .filter(|(index, _)| {
            let one_based = index.saturating_add(1);
            one_based >= start && one_based <= line.saturating_add(2)
        })
        .map(|(index, content)| format!("{}:{content}", index.saturating_add(1)))
        .collect::<Vec<_>>()
        .join("\\n");
    format!("{error}; source excerpt: {excerpt}")
}

/// Reads one bounded length-prefixed frame.
///
/// # Errors
///
/// Returns a framing or I/O error for zero, partial, or oversized frames.
pub fn read_frame<R: Read>(
    reader: &mut R,
    limits: FrameLimits,
) -> Result<Option<Vec<u8>>, SolverError> {
    let mut header = [0_u8; 4];
    let first = reader
        .read(&mut header[..1])
        .map_err(|error| SolverError::Io(error.to_string()))?;
    if first == 0 {
        return Ok(None);
    }
    reader
        .read_exact(&mut header[1..])
        .map_err(|error| SolverError::Frame(format!("partial header: {error}")))?;
    let declared = u32::from_be_bytes(header) as usize;
    if declared == 0 {
        return Err(SolverError::Frame(FrameError::ZeroLength.to_string()));
    }
    if declared > limits.maximum_frame_bytes {
        return Err(SolverError::Frame(
            FrameError::Oversized {
                declared,
                maximum: limits.maximum_frame_bytes,
            }
            .to_string(),
        ));
    }
    let mut payload = vec![0_u8; declared];
    reader
        .read_exact(&mut payload)
        .map_err(|error| SolverError::Frame(format!("partial payload: {error}")))?;
    Ok(Some(payload))
}

/// Writes one bounded length-prefixed frame and flushes it.
///
/// # Errors
///
/// Returns a framing or I/O error for empty, oversized, or unwritable frames.
pub fn write_frame<W: Write>(
    writer: &mut W,
    payload: &[u8],
    limits: FrameLimits,
) -> Result<(), SolverError> {
    if payload.is_empty() {
        return Err(SolverError::Frame(FrameError::ZeroLength.to_string()));
    }
    if payload.len() > limits.maximum_frame_bytes {
        return Err(SolverError::Frame(
            FrameError::Oversized {
                declared: payload.len(),
                maximum: limits.maximum_frame_bytes,
            }
            .to_string(),
        ));
    }
    let declared = u32::try_from(payload.len())
        .map_err(|_| SolverError::Frame("payload length exceeds u32".to_owned()))?;
    writer
        .write_all(&declared.to_be_bytes())
        .and_then(|()| writer.write_all(payload))
        .and_then(|()| writer.flush())
        .map_err(|error| SolverError::Io(error.to_string()))
}

#[must_use]
#[allow(clippy::needless_pass_by_value)]
pub fn io_error(error: io::Error) -> SolverError {
    SolverError::Io(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_key_contains_every_behavior_affecting_input() {
        let input = WorkerInput {
            challenge: ChallengeKind::N,
            value: "ignored-by-cache-key".to_owned(),
            script_sha256: "a".repeat(64),
            extractor_version: "extractor-v1".to_owned(),
        };
        let baseline = CacheKey::from_input(&input);
        let mut signature = input.clone();
        signature.challenge = ChallengeKind::Signature;
        assert_ne!(baseline, CacheKey::from_input(&signature));
        let mut script = input.clone();
        script.script_sha256 = "b".repeat(64);
        assert_ne!(baseline, CacheKey::from_input(&script));
        let mut extractor = input;
        extractor.extractor_version = "extractor-v2".to_owned();
        assert_ne!(baseline, CacheKey::from_input(&extractor));
    }

    #[test]
    fn cache_is_bounded_by_entries_and_bytes() {
        let mut cache = WorkerCache::new(1, 8);
        let input = WorkerInput {
            challenge: ChallengeKind::N,
            value: String::new(),
            script_sha256: "a".repeat(64),
            extractor_version: "v1".to_owned(),
        };
        let first_key = CacheKey::from_input(&input);
        cache.insert(
            first_key.clone(),
            PreparedProgram {
                source: "1234".to_owned(),
                source_sha256: "a".repeat(64),
                candidate_count: 1,
            },
        );
        let mut second = input;
        second.script_sha256 = "b".repeat(64);
        let second_key = CacheKey::from_input(&second);
        cache.insert(
            second_key.clone(),
            PreparedProgram {
                source: "5678".to_owned(),
                source_sha256: "b".repeat(64),
                candidate_count: 1,
            },
        );
        assert!(cache.get(&first_key).is_none());
        assert!(cache.get(&second_key).is_some());
        assert_eq!(cache.len(), 1);
        assert_eq!(cache.total_bytes(), 4);
    }

    #[test]
    fn framing_rejects_zero_oversized_and_partial_inputs() {
        let limits = FrameLimits {
            maximum_frame_bytes: 4,
        };
        assert!(matches!(
            read_frame(&mut &0_u32.to_be_bytes()[..], limits),
            Err(SolverError::Frame(_))
        ));
        assert!(matches!(
            read_frame(&mut &5_u32.to_be_bytes()[..], limits),
            Err(SolverError::Frame(_))
        ));
        assert!(matches!(
            read_frame(&mut &[0_u8, 0, 0][..], limits),
            Err(SolverError::Frame(_))
        ));
        assert!(matches!(
            read_frame(&mut &[0_u8, 0, 0, 2, b'x'][..], limits),
            Err(SolverError::Frame(_))
        ));
    }

    #[test]
    fn fresh_probe_context_does_not_retain_heap_state() -> Result<(), SolverError> {
        let base = WorkerInput {
            challenge: ChallengeKind::StateSeed,
            value: String::new(),
            script_sha256: "0".repeat(64),
            extractor_version: "probe-v1".to_owned(),
        };
        assert_eq!(execute_probe(&base)?.result, "seeded");
        let check = WorkerInput {
            challenge: ChallengeKind::StateCheck,
            ..base
        };
        assert_eq!(execute_probe(&check)?.result, "undefined");
        Ok(())
    }

    #[test]
    fn capability_probe_exposes_no_ambient_runtime_surface() -> Result<(), SolverError> {
        let result = execute_probe(&WorkerInput {
            challenge: ChallengeKind::CapabilityProbe,
            value: String::new(),
            script_sha256: "0".repeat(64),
            extractor_version: "probe-v1".to_owned(),
        })?;
        let value: serde_json::Value = serde_json::from_str(&result.result)
            .map_err(|error| SolverError::InvalidResult(error.to_string()))?;
        for name in ["fetch", "require", "process", "deno", "bun", "console"] {
            assert_eq!(value[name], "undefined");
        }
        Ok(())
    }

    #[test]
    fn structural_discovery_skips_prelude_iife_and_unrelated_markers() {
        let program = br#"
(function(){var a="alr";var b="yes";var fake=function(value){return value;};})();
(function(){Real=function(url,key,value){var a="alr",b="yes";return url;};})();
"#;
        let prepared = prepare_player(program).expect("second IIFE contains the candidate shape");
        assert_eq!(prepared.candidate_count, 1);
        assert!(prepared.source.contains("run:Real"));
        assert!(!prepared.source.contains("run:fake"));
    }

    #[test]
    fn unrelated_marker_literals_do_not_create_candidate() {
        let program = br#"
(function(){var a="alr";var b="yes";var fake=function(value){return value;};})();
"#;
        assert!(matches!(
            prepare_player(program),
            Err(SolverError::NoChallengeCandidate)
        ));
    }

    #[test]
    fn source_spans_accept_crlf_player_input() {
        let program =
            b"(function(){\r\nReal=function(url,key,value){var a=\"alr\",b=\"yes\";return url;};\r\n})();";
        let prepared = prepare_player(program).expect("CRLF spans remain byte exact");
        assert_eq!(prepared.candidate_count, 1);
    }
}
