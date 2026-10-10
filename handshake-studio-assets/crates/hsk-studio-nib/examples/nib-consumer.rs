use hsk_studio_accord::{
    ActorContext, CancellationToken, DomainId, Length, SCHEMA_STUDIO_VECTOR_PATH, Unit,
};
use hsk_studio_nib::*;
use hsk_studio_observe::{
    Budget, DeliveryClass, DeliveryError, MAX_FRAME_BYTES, Observe, SinkPort,
};
use sha2::{Digest, Sha256};
use std::sync::{
    Mutex,
    atomic::{AtomicBool, AtomicU64, Ordering},
};

pub fn digest(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}
pub fn id(prefix: &str, last: u8) -> DomainId {
    DomainId::parse(&format!("{prefix}-00000000-0000-7000-8000-{last:012x}"))
        .expect("valid preowned ID")
}
pub fn default_limits() -> Limits {
    Limits {
        input_bytes: 65536,
        operands: 2,
        anchors: 8,
        segments: 8,
        sweep_events: 240,
        intersections: 56,
        output_vertices: 64,
        output_segments: 120,
        output_regions: 120,
        output_loops: 120,
        work_units: 1_000_000,
        recursion_depth: 8,
        requested_allocation_bytes: 2_000_000,
    }
}
pub fn rectangle(coords: [f64; 4]) -> Result<[Anchor; 4], Error> {
    let zero = Point::pt(0.0, 0.0)?;
    let points = [
        [coords[0], coords[1]],
        [coords[2], coords[1]],
        [coords[2], coords[3]],
        [coords[0], coords[3]],
    ];
    let mut out = [Anchor {
        position: zero,
        incoming: zero,
        outgoing: zero,
        handle_mirroring: Mirroring::None,
    }; 4];
    for (i, p) in points.into_iter().enumerate() {
        out[i].position = Point::pt(p[0], p[1])?;
    }
    Ok(out)
}
fn point_json(p: Point) -> String {
    format!(
        "{{\"x\":{{\"value\":{},\"unit\":\"{}\"}},\"y\":{{\"value\":{},\"unit\":\"{}\"}}}}",
        p.x.value(),
        p.x.unit().as_str(),
        p.y.value(),
        p.y.unit().as_str()
    )
}
pub fn encoded(path: &Path<'_>) -> Vec<u8> {
    let mut anchors = String::new();
    for (i, a) in path.anchors.iter().enumerate() {
        if i != 0 {
            anchors.push(',');
        }
        anchors.push_str(&format!(
            "{{\"position\":{},\"incoming\":{},\"outgoing\":{},\"handle_mirroring\":\"{}\"}}",
            point_json(a.position),
            point_json(a.incoming),
            point_json(a.outgoing),
            a.handle_mirroring.wire()
        ));
    }
    let style = path
        .fill_style_id
        .map(|id| format!("\"{}\"", id.as_str()))
        .unwrap_or_else(|| "null".into());
    format!("{{\"schema_id\":\"{}\",\"path_id\":\"{}\",\"address\":{{\"layer_id\":\"{}\",\"object_key\":\"{}\",\"property\":\"geometry\"}},\"closed\":{},\"anchors\":[{}],\"winding_rule\":\"{}\",\"fill_style_id\":{}}}",path.schema_id,path.path_id.as_str(),path.address.layer_id.as_str(),path.address.object_key,path.closed,anchors,path.winding_rule.wire(),style).into_bytes()
}
/// Actual aggregate caller ledger: unique preowned inputs plus live output grants.
pub struct Meter {
    pub current: AtomicU64,
    pub peak: AtomicU64,
    pub grants: AtomicU64,
    pub retired: AtomicU64,
    pub denied: AtomicBool,
    pub input_bytes: u64,
}
impl AdmissionPort for Meter {
    fn reserve(&self, bytes: u64, limit: u64) -> Result<Reservation<'_>, Error> {
        if self.denied.load(Ordering::SeqCst) {
            return Err(Error::LeaseUnavailable);
        }
        let mut old = self.current.load(Ordering::SeqCst);
        loop {
            let next = old.checked_add(bytes).ok_or(Error::Overflow)?;
            if next > limit {
                return Err(Error::BudgetExceeded);
            }
            match self
                .current
                .compare_exchange(old, next, Ordering::SeqCst, Ordering::SeqCst)
            {
                Ok(_) => {
                    self.peak.fetch_max(next, Ordering::SeqCst);
                    break;
                }
                Err(actual) => old = actual,
            }
        }
        let handle = self.grants.fetch_add(1, Ordering::SeqCst) + 1;
        Reservation::new(self, handle, bytes)
    }
}
impl RetirementPort for Meter {
    fn retire(&self, handle: u64, bytes: u64) {
        assert!(handle > 0);
        let old = self.current.fetch_sub(bytes, Ordering::SeqCst);
        assert!(old >= self.input_bytes + bytes);
        self.retired.fetch_add(1, Ordering::SeqCst);
    }
}
pub struct Scene {
    pub document: DomainId,
    pub layer: DomainId,
    pub paths: [DomainId; 2],
    pub styles: [DomainId; 3],
    pub actor: ActorContext,
    pub anchors: [[Anchor; 4]; 2],
    pub bytes: [Vec<u8>; 2],
    pub closed: [bool; 2],
    pub winding: [Winding; 2],
    pub geometry_revision: [AtomicU64; 2],
    pub style_revision: AtomicU64,
    pub order_revision: AtomicU64,
    pub target_revision: AtomicU64,
    pub epoch: AtomicU64,
    pub missing: AtomicBool,
    pub unsupported_style: AtomicBool,
    pub meter: Meter,
    pub lock: Mutex<()>,
    pub cancel: CancellationToken,
}
const STYLE_BYTES: &[u8] = b"{\"kind\":\"PAINT\",\"fill_stack\":[\"original-private-paint\"]}";
impl Scene {
    pub fn new(a: [f64; 4], b: [f64; 4]) -> Result<Self, Error> {
        let actor = ActorContext::new(
            "caller-account",
            "caller-principal",
            "owner-account",
            "owner-principal",
            "access-space",
            "session",
        )
        .map_err(|_| Error::InvalidRequest)?;
        let mut scene = Self {
            document: id("SDOC", 1),
            layer: id("SLYR", 2),
            paths: [id("SVPT", 3), id("SVPT", 4)],
            styles: [id("SSTY", 5), id("SSTY", 6), id("SSTY", 7)],
            actor,
            anchors: [rectangle(a)?, rectangle(b)?],
            bytes: [Vec::new(), Vec::new()],
            closed: [true; 2],
            winding: [Winding::NonZero; 2],
            geometry_revision: [AtomicU64::new(10), AtomicU64::new(10)],
            style_revision: AtomicU64::new(20),
            order_revision: AtomicU64::new(30),
            target_revision: AtomicU64::new(40),
            epoch: AtomicU64::new(1),
            missing: AtomicBool::new(false),
            unsupported_style: AtomicBool::new(false),
            meter: Meter {
                current: AtomicU64::new(0),
                peak: AtomicU64::new(0),
                grants: AtomicU64::new(0),
                retired: AtomicU64::new(0),
                denied: AtomicBool::new(false),
                input_bytes: 0,
            },
            lock: Mutex::new(()),
            cancel: CancellationToken::default(),
        };
        scene.refresh();
        Ok(scene)
    }
    pub fn path(&self, index: usize) -> Path<'_> {
        Path {
            schema_id: SCHEMA_STUDIO_VECTOR_PATH,
            path_id: &self.paths[index],
            address: GeometryAddress {
                layer_id: &self.layer,
                object_key: if index == 0 { "operand_a" } else { "operand_b" },
            },
            closed: self.closed[index],
            anchors: &self.anchors[index],
            winding_rule: self.winding[index],
            fill_style_id: Some(&self.styles[index]),
        }
    }
    /// Fixture setup only, before the public operation and allocation capture.
    pub fn refresh(&mut self) {
        self.bytes = [encoded(&self.path(0)), encoded(&self.path(1))];
        self.precharge();
    }
    pub fn precharge(&mut self) {
        // String constructor requests exact string lengths; Vec requests its capacity.
        let ids = self.document.as_str().len()
            + self.layer.as_str().len()
            + self.paths.iter().map(|x| x.as_str().len()).sum::<usize>()
            + self.styles.iter().map(|x| x.as_str().len()).sum::<usize>();
        let actor = self.actor.account_id().len()
            + self.actor.principal_id().len()
            + self.actor.owner_account_id().len()
            + self.actor.owner_principal_id().len()
            + self.actor.access_space_id().len()
            + self.actor.session_id().len();
        #[repr(C, align(2))]
        struct Header {
            strong: std::sync::atomic::AtomicUsize,
            weak: std::sync::atomic::AtomicUsize,
            data: (),
        }
        let cancel_layout = std::alloc::Layout::new::<Header>()
            .extend(std::alloc::Layout::new::<AtomicBool>())
            .expect("valid inline target layouts")
            .0
            .pad_to_align()
            .size();
        self.meter.input_bytes =
            (self.bytes[0].capacity() + self.bytes[1].capacity() + ids + actor + cancel_layout)
                as u64;
        self.meter
            .current
            .store(self.meter.input_bytes, Ordering::SeqCst);
        self.meter
            .peak
            .store(self.meter.input_bytes, Ordering::SeqCst);
    }
    pub fn observer(&self) -> Observe {
        Observe::new(
            99,
            40,
            self.layer.clone(),
            self.actor.clone(),
            Budget::new(0, 0).expect("fixed terminal budget"),
        )
    }
    pub fn with_request<T>(
        &self,
        operation: Operation,
        permuted: bool,
        call: impl FnOnce(Request<'_>, Context<'_>) -> T,
    ) -> T {
        let operands = [
            Operand {
                path: self.path(if permuted { 1 } else { 0 }),
                encoded_path_bytes: &self.bytes[if permuted { 1 } else { 0 }],
                expected_revision: 10,
                geometry_sha256: digest(&self.bytes[if permuted { 1 } else { 0 }]),
            },
            Operand {
                path: self.path(if permuted { 0 } else { 1 }),
                encoded_path_bytes: &self.bytes[if permuted { 0 } else { 1 }],
                expected_revision: 10,
                geometry_sha256: digest(&self.bytes[if permuted { 0 } else { 1 }]),
            },
        ];
        let order = [&self.paths[0], &self.paths[1]];
        let styles = self.styles.each_ref().map(|style| StyleRead {
            style_id: style,
            expected_revision: 20,
            expected_fingerprint_sha256: digest(STYLE_BYTES),
        });
        let request = Request {
            transport_version: 1,
            document_id: &self.document,
            command_id: "rectangle_boolean",
            correlation_id: 99,
            actor: &self.actor,
            base_revision: 1,
            cancel_epoch: 1,
            result_mode: ResultMode::ProposalOnly,
            operation,
            operands: &operands,
            z_order: &order,
            expected_z_order_revision: 30,
            target: Target {
                address: GeometryAddress {
                    layer_id: &self.layer,
                    object_key: "result",
                },
                expected_revision: 40,
            },
            options: Options {
                precision: Length::new(0.01, Unit::Points).expect("finite precision"),
                remove_redundant_points: false,
                divide_and_outline_remove_unpainted: false,
                loss_policy: LossPolicy::Reject,
            },
            style_reads: &styles,
            result_fill_style_id: &self.styles[2],
            limits: default_limits(),
        };
        call(
            request,
            Context {
                scene: self,
                order: &order,
            },
        )
    }
}
impl InputLifetime for Scene {
    fn verify(&self, request: &Request<'_>) -> Result<InputCharge, Error> {
        if request.document_id != &self.document
            || self.meter.current.load(Ordering::SeqCst) < self.meter.input_bytes
        {
            return Err(Error::LeaseUnavailable);
        }
        for o in request.operands {
            let i = self
                .paths
                .iter()
                .position(|id| id == o.path.path_id)
                .ok_or(Error::AbsentRead)?;
            if o.encoded_path_bytes.as_ptr() != self.bytes[i].as_ptr()
                || o.path.anchors.as_ptr() != self.anchors[i].as_ptr()
            {
                return Err(Error::LeaseUnavailable);
            }
        }
        Ok(InputCharge {
            lifetime_id: 1,
            requested_bytes: self.meter.input_bytes,
        })
    }
}
pub struct Context<'a> {
    pub scene: &'a Scene,
    pub order: &'a [&'a DomainId],
}
impl EpochPort for Context<'_> {
    fn current_epoch(&self) -> Result<u64, Error> {
        Ok(self.scene.epoch.load(Ordering::SeqCst))
    }
}
impl ContextPort for Context<'_> {
    fn member(&self, request: &Request<'_>, operand: &Operand<'_>) -> Result<(), Error> {
        if self.scene.missing.load(Ordering::SeqCst) {
            return Err(Error::AbsentRead);
        }
        if request.document_id != &self.scene.document
            || !self.scene.paths.iter().any(|id| id == operand.path.path_id)
        {
            return Err(Error::AbsentRead);
        }
        Ok(())
    }
    fn geometry(&self, operand: &Operand<'_>) -> Result<GeometryRead<'_>, Error> {
        let i = self
            .scene
            .paths
            .iter()
            .position(|id| id == operand.path.path_id)
            .ok_or(Error::UnavailableRead)?;
        Ok(GeometryRead {
            revision: self.scene.geometry_revision[i].load(Ordering::SeqCst),
            encoded_path_bytes: &self.scene.bytes[i],
        })
    }
    fn order(&self, _: &Request<'_>) -> Result<OrderRead<'_>, Error> {
        Ok(OrderRead {
            revision: self.scene.order_revision.load(Ordering::SeqCst),
            bottom_to_top: self.order,
        })
    }
    fn style(&self, read: &StyleRead<'_>) -> Result<ResolvedStyle<'_>, Error> {
        if !self.scene.styles.iter().any(|id| id == read.style_id) {
            return Err(Error::UnavailableStyle);
        }
        Ok(ResolvedStyle {
            kind: if self.scene.unsupported_style.load(Ordering::SeqCst) {
                StyleKind::Unsupported
            } else {
                StyleKind::Paint
            },
            revision: self.scene.style_revision.load(Ordering::SeqCst),
            encoded_record_bytes: STYLE_BYTES,
        })
    }
    fn target_revision(&self, _: &Request<'_>) -> Result<u64, Error> {
        Ok(self.scene.target_revision.load(Ordering::SeqCst))
    }
}
pub struct Collector {
    pub frames: [[u8; MAX_FRAME_BYTES]; 4],
    pub lengths: [usize; 4],
    pub count: usize,
    pub mode: Option<DeliveryError>,
}
impl Collector {
    pub fn new(mode: Option<DeliveryError>) -> Self {
        Self {
            frames: [[0; MAX_FRAME_BYTES]; 4],
            lengths: [0; 4],
            count: 0,
            mode,
        }
    }
}
impl SinkPort for Collector {
    fn try_send(&mut self, class: DeliveryClass, bytes: &[u8]) -> Result<(), DeliveryError> {
        if class != DeliveryClass::Terminal || self.count == 4 || bytes.len() > MAX_FRAME_BYTES {
            return Err(DeliveryError::Rejected);
        }
        if matches!(
            self.mode,
            Some(DeliveryError::Rejected | DeliveryError::Unavailable | DeliveryError::Saturated)
        ) {
            return Err(self.mode.expect("checked mode"));
        }
        self.frames[self.count][..bytes.len()].copy_from_slice(bytes);
        self.lengths[self.count] = bytes.len();
        self.count += 1;
        if self.mode == Some(DeliveryError::Indeterminate) {
            Err(DeliveryError::Indeterminate)
        } else {
            Ok(())
        }
    }
}
pub struct Port<'a> {
    pub context: &'a Context<'a>,
    pub sink: Collector,
    pub reconciliation: Reconciliation,
    pub exclusive_error: Option<Error>,
}
impl FinalizationPort for Port<'_> {
    fn with_exclusive<T>(
        &mut self,
        call: impl FnOnce(&dyn ContextPort, &mut dyn SinkPort) -> T,
    ) -> Result<T, Error> {
        if let Some(error) = self.exclusive_error {
            return Err(error);
        }
        let _guard = self
            .context
            .scene
            .lock
            .lock()
            .map_err(|_| Error::UnsupportedFinalization)?;
        Ok(call(self.context, &mut self.sink))
    }
    fn reconcile_delivery(&mut self, _: &PendingKey) -> Result<Reconciliation, Error> {
        let _guard = self
            .context
            .scene
            .lock
            .lock()
            .map_err(|_| Error::UnsupportedFinalization)?;
        Ok(self.reconciliation)
    }
}
pub fn area(network: &Network<'_>) -> f64 {
    network
        .loops()
        .iter()
        .enumerate()
        .map(|(index, _)| {
            network
                .loop_segment_indices(index)
                .expect("indices")
                .iter()
                .map(|&index| {
                    let s = &network.segments()[index];
                    let a = network.vertices()[s.start()];
                    let b = network.vertices()[s.end()];
                    a.x.value() * b.y.value() - b.x.value() * a.y.value()
                })
                .sum::<f64>()
                / 2.0
        })
        .sum()
}
pub fn print_network(network: &Network<'_>) {
    println!("{{\"area_pt2\":{},\"vertices\":[", area(network));
    for (i, p) in network.vertices().iter().enumerate() {
        println!(
            "{}[{},{}]",
            if i == 0 { "" } else { "," },
            p.x.value(),
            p.y.value()
        );
    }
    println!("],\"segments\":[");
    for (i, s) in network.segments().iter().enumerate() {
        println!(
            "{}[{},{}]",
            if i == 0 { "" } else { "," },
            s.start(),
            s.end()
        );
    }
    println!("],\"loops\":[");
    for i in 0..network.loops().len() {
        print!("{}[", if i == 0 { "" } else { "," });
        for (j, n) in network
            .loop_segment_indices(i)
            .expect("loop")
            .iter()
            .enumerate()
        {
            print!("{}{n}", if j == 0 { "" } else { "," });
        }
        println!("]");
    }
    println!("],\"regions\":[");
    for (i, region) in network.regions().iter().enumerate() {
        let range = region.loop_range();
        println!(
            "{}{{\"loop_start\":{},\"loop_count\":{},\"fill_style_id\":\"{}\",\"winding_rule\":\"NONZERO\"}}",
            if i == 0 { "" } else { "," },
            range.start,
            range.len(),
            region.fill_style_id().as_str()
        );
    }
    println!("]}}");
}
pub fn print_frames(sink: &Collector) {
    for i in 0..sink.count {
        print!("observe_frame_hex:");
        for b in &sink.frames[i][..sink.lengths[i]] {
            print!("{b:02x}");
        }
        println!();
    }
}
#[cfg(not(test))]
fn main() {
    if let Err(error) = run() {
        eprintln!("nib_consumer_error:{error}");
        std::process::exit(2);
    }
}
#[cfg(not(test))]
fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.as_slice() == ["--descriptor"] {
        println!("{}", hsk_studio_nib::descriptor());
        return Ok(());
    }
    if args.len() != 16
        || args[0] != "--operation"
        || args[2] != "--rects"
        || args[11] != "--path-a"
        || args[13] != "--path-b"
        || args[15] != "--execute"
    {
        return Err("use --descriptor, or --operation OP --rects ax0 ay0 ax1 ay1 bx0 by0 bx1 by1 --path-a FILE --path-b FILE --execute".into());
    }
    let operation = Operation::parse(&args[1]).map_err(|e| format!("{e:?}"))?;
    let mut numbers = [0.0; 8];
    for (i, n) in numbers.iter_mut().enumerate() {
        *n = args[i + 3].parse().map_err(|_| "invalid coordinate")?;
    }
    let mut scene = Scene::new(
        numbers[..4].try_into().expect("four"),
        numbers[4..].try_into().expect("four"),
    )
    .map_err(|e| format!("{e:?}"))?;
    scene.bytes = [
        std::fs::read(&args[12]).map_err(|e| e.to_string())?,
        std::fs::read(&args[14]).map_err(|e| e.to_string())?,
    ];
    scene.precharge();
    scene.with_request(operation,false,|request,context|{
        let cancel=&scene.cancel;let mut observer=scene.observer();let mut diagnostics=scene.observer();let mut fallback=Collector::new(None);
        let proposal=match prepare(request,&context,&scene,&scene.meter,cancel){Ok(value)=>value,Err(rejection)=>{let delivery=emit_rejection(&request,rejection,&mut observer,cancel,&mut fallback);print_frames(&fallback);println!("{{\"disposition\":\"rejected\",\"code\":\"{:?}\",\"work_units\":{},\"diagnostic_delivered\":{}}}",rejection.error(),rejection.inspection.counts.work_units,delivery.is_ok());return Err(format!("{:?}",rejection.error()));}};
        let mut port=Port{context:&context,sink:Collector::new(None),reconciliation:Reconciliation::StillIndeterminate,exclusive_error:None};
        let outcome=finalize(proposal,&mut port,&mut observer,&mut diagnostics,cancel,&mut fallback);
        print_frames(&port.sink);print_frames(&fallback);
        match outcome{Finalized::Accepted{proposal,inspection}=>{println!("{{\"disposition\":\"accepted_source_proposal\",\"work_units\":{},\"retained_bytes\":{}}}",inspection.counts.work_units,inspection.counts.retained_owned_bytes);print_network(proposal.geometry());},other=>return Err(format!("{:?}",other.inspection().error))}Ok(())
    })
}
