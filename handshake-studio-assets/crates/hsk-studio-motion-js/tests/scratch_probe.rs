#![cfg(feature = "quickjs")]
use hsk_studio_accord::CancellationToken;
use hsk_studio_motion::*;
use hsk_studio_motion_js::*;

struct Host;
impl HostRead for Host {
    fn read(&self, _p: &PropertyPath, _t: EvalTick, fuel: &mut Fuel) -> Result<Vec<f64>, ExprError> {
        fuel.charge(1)?;
        Ok(vec![1.0])
    }
}

fn run(source: &str) {
    let quotas = Quotas::try_from_spec(QuotaSpec::default()).unwrap();
    let provider = MotionJsProvider::new(QuickJsEngine::new(&quotas).unwrap(), quotas);
    let request = ExprRequest { source, revision: 0, tick: EvalTick::new(0), seed: 0, dimension: 1, base: None };
    let result = provider.evaluate(&request, &Host, &mut Fuel::new(1000), &CancellationToken::default());
    println!("RESULT {source:?} -> {result:?}");
    let report = provider.retire();
    println!("RETIRE {report:?}");
}

#[test]
fn c01_async() {
    run("async function f(){}; 1");
}

#[test]
fn c02_async_call() {
    run("(async () => 1)(); 1");
}

#[test]
fn c03_asyncgen() {
    run("async function* g(){}; 1");
}

#[test]
fn c04_class() {
    run("class A { get y(){return 1} static z = 2 }; new A().y + A.z");
}

#[test]
fn c05_generator() {
    run("function* g(){ yield 1; yield 2 }; [...g()].length");
}

#[test]
fn c06_bigint() {
    run("1n + 2n");
}

#[test]
fn c07_template() {
    run("`a${1}b`.length");
}

#[test]
fn c08_symbol() {
    run("Symbol(\"x\").toString().length");
}

#[test]
fn c09_import() {
    run("import(\"x\"); 1");
}

#[test]
fn c10_label() {
    run("l: for(;;){ break l }; 1");
}

#[test]
fn c11_newtarget() {
    run("new.target");
}

#[test]
fn c12_getter() {
    run("({get a(){return 1}}).a");
}

#[test]
fn c13_arrow_map() {
    run("[1,2,3].map(x=>x*2).length");
}

#[test]
fn c14_proto_pollute() {
    run("Object.defineProperty(Array.prototype,\"0\",{get(){throw 1}}); [5]");
}

#[test]
fn c15_typeofs() {
    run("(typeof Promise + typeof Proxy + typeof Reflect + typeof JSON + typeof RegExp + typeof Date + typeof WeakRef + typeof Map + typeof Set + typeof globalThis.gc).length");
}

#[test]
fn c16_cycle() {
    run("var a=[]; a[0]=a; a");
}

#[test]
fn c17_global_getter() {
    run("Object.defineProperty(globalThis,\"x\",{get(){ throw 1 }}); x");
}

#[test]
fn c18_stack() {
    run("function f(){return f()+1} f()");
}

#[test]
fn c19_oom() {
    run("\"x\".repeat(100000000).length");
}

#[test]
fn c20_promise_then() {
    run("(async function(){ return 1 })().then(function(){}); 1");
}

#[test]
fn c21_spread_destructure() {
    run("var [a,b]=[1,2]; var {c}={c:3}; a+b+c");
}

#[test]
fn c22_optional() {
    run("var o=null; (o?.x ?? 4)");
}

#[test]
fn c23_regex() {
    run("/a/.test(\"a\") ? 1 : 0");
}

#[test]
fn c24_closure_fuel() {
    run("var s=0; for(var i=0;i<50000;i++){s+=i}; s");
}

#[test]
fn c25_funcproto() {
    run("(function(){}).__proto__.constructor === Function");
}
