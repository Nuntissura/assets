#![cfg(feature = "quickjs")]
use rquickjs::{Context, Function, Runtime, Value, context::intrinsic, function::Opt};
use std::{cell::Cell, rc::Rc};

fn ctx_eval(rt: &Runtime) -> Context {
    Context::builder().with::<intrinsic::Eval>().build(rt).unwrap()
}

#[test]
fn a_plain() {
    let rt = Runtime::new().unwrap();
    let ctx = ctx_eval(&rt);
    let v: f64 = ctx.with(|ctx| ctx.eval("1+2").unwrap());
    println!("a {v}");
    drop(ctx);
    drop(rt);
    println!("a dropped");
}

#[test]
fn b_host_fn() {
    let rt = Runtime::new().unwrap();
    let ctx = ctx_eval(&rt);
    let shared = Rc::new(Cell::new(0u32));
    ctx.with(|ctx| {
        let s = Rc::clone(&shared);
        let f = Function::new(ctx.clone(), move |p: String, t: Opt<f64>| -> Vec<f64> {
            s.set(s.get() + 1);
            let _ = (p, t);
            Vec::new()
        })
        .unwrap();
        ctx.globals().set("hostRead", f).unwrap();
        let v: Value = ctx.eval("hostRead('x')").unwrap();
        println!("b {:?}", v.is_array());
    });
    drop(ctx);
    drop(rt);
    println!("b dropped");
}

#[test]
fn c_strict_options() {
    let rt = Runtime::new().unwrap();
    let ctx = ctx_eval(&rt);
    ctx.with(|ctx| {
        let mut o = rquickjs::context::EvalOptions::default();
        o.strict = true;
        o.filename = Some("expr".to_owned());
        let v: f64 = ctx.eval_with_options("1+2", o).unwrap();
        println!("c {v}");
    });
    drop(ctx);
    drop(rt);
    println!("c dropped");
}

#[test]
fn d_interrupt() {
    let rt = Runtime::new().unwrap();
    rt.set_interrupt_handler(Some(Box::new(|| false)));
    let ctx = ctx_eval(&rt);
    ctx.with(|ctx| {
        let v: f64 = ctx.eval("1+2").unwrap();
        println!("d {v}");
    });
    drop(ctx);
    drop(rt);
    println!("d dropped");
}

#[test]
fn e_limits() {
    let rt = Runtime::new().unwrap();
    rt.set_memory_limit(16 << 20);
    rt.set_max_stack_size(512 << 10);
    let ctx = ctx_eval(&rt);
    ctx.with(|ctx| {
        let v: f64 = ctx.eval("1+2").unwrap();
        println!("e {v}");
    });
    drop(ctx);
    drop(rt);
    println!("e dropped");
}

const BOOT: &str = r#"(function (g) {
  "use strict";
  var def = Object.defineProperty;
  var getProto = Object.getPrototypeOf;
  var gopd = Object.getOwnPropertyDescriptor;
  var isArray = Array.isArray;
  var TypeErr = TypeError;
  function deny(obj, key, what) {
    var thrower = function () { throw new TypeErr("motionjs-unavailable: " + what); };
    def(obj, key, { value: thrower, writable: false, enumerable: false, configurable: false });
  }
  function protoOf(make) {
    try { return getProto(make()); } catch (e) { return null; }
  }
  var protos = [
    Function.prototype,
    protoOf(function () { return function* () {}; }),
    protoOf(function () { return async function () {}; }),
    protoOf(function () { return async function* () {}; })
  ];
  for (var i = 0; i < protos.length; i++) {
    if (protos[i] !== null) { deny(protos[i], "constructor", "dynamic function constructor"); }
  }
  deny(g, "Function", "Function");
  deny(g, "eval", "eval");
  deny(Math, "random", "Math.random");
  function extract(v, max) {
    if (typeof v === "number") { return v; }
    if (!isArray(v)) { return undefined; }
    var n = v.length;
    if (typeof n !== "number" || n > max) { return undefined; }
    var out = [];
    for (var k = 0; k < n; k++) {
      var d = gopd(v, k);
      if (d === undefined || gopd(d, "value") === undefined) { return undefined; }
      if (typeof d.value !== "number") { return undefined; }
      def(out, k, { value: d.value, writable: true, enumerable: true, configurable: true });
    }
    return out;
  }
  return extract;
})(globalThis)"#;

#[test]
fn f_bootstrap_only() {
    let rt = Runtime::new().unwrap();
    let ctx = ctx_eval(&rt);
    ctx.with(|ctx| {
        let f: Function = ctx.eval(BOOT).unwrap();
        println!("f got function {}", f.is_function());
    });
    drop(ctx);
    drop(rt);
    println!("f dropped");
}

#[test]
fn g_bootstrap_no_extract_held() {
    let rt = Runtime::new().unwrap();
    let ctx = ctx_eval(&rt);
    ctx.with(|ctx| {
        let _: Value = ctx.eval(BOOT).unwrap();
    });
    drop(ctx);
    drop(rt);
    println!("g dropped");
}

#[test]
fn h_bootstrap_then_user() {
    let rt = Runtime::new().unwrap();
    let ctx = ctx_eval(&rt);
    ctx.with(|ctx| {
        let f: Function = ctx.eval(BOOT).unwrap();
        let v: f64 = ctx.eval("1+2").unwrap();
        let x: Value = f.call((v, 10usize)).unwrap();
        println!("h {:?}", x.as_number());
    });
    drop(ctx);
    drop(rt);
    println!("h dropped");
}

#[test]
fn v1_fp_ctor() {
    let rt = Runtime::new().unwrap();
    let ctx = ctx_eval(&rt);
    ctx.with(|ctx| {
        let v: f64 = ctx.eval("Object.defineProperty(Function.prototype,\"constructor\",{value:function(){},writable:false,enumerable:false,configurable:false}); 1").unwrap();
        println!("v1_fp_ctor {v}");
    });
    drop(ctx);
    drop(rt);
    println!("v1_fp_ctor dropped");
}

#[test]
fn v2_global_function() {
    let rt = Runtime::new().unwrap();
    let ctx = ctx_eval(&rt);
    ctx.with(|ctx| {
        let v: f64 = ctx.eval("Object.defineProperty(globalThis,\"Function\",{value:function(){},writable:false,enumerable:false,configurable:false}); 1").unwrap();
        println!("v2_global_function {v}");
    });
    drop(ctx);
    drop(rt);
    println!("v2_global_function dropped");
}

#[test]
fn v3_global_eval() {
    let rt = Runtime::new().unwrap();
    let ctx = ctx_eval(&rt);
    ctx.with(|ctx| {
        let v: f64 = ctx.eval("Object.defineProperty(globalThis,\"eval\",{value:function(){},writable:false,enumerable:false,configurable:false}); 1").unwrap();
        println!("v3_global_eval {v}");
    });
    drop(ctx);
    drop(rt);
    println!("v3_global_eval dropped");
}

#[test]
fn v4_math_random() {
    let rt = Runtime::new().unwrap();
    let ctx = ctx_eval(&rt);
    ctx.with(|ctx| {
        let v: f64 = ctx.eval("Object.defineProperty(Math,\"random\",{value:function(){},writable:false,enumerable:false,configurable:false}); 1").unwrap();
        println!("v4_math_random {v}");
    });
    drop(ctx);
    drop(rt);
    println!("v4_math_random dropped");
}

#[test]
fn v5_gen_protos() {
    let rt = Runtime::new().unwrap();
    let ctx = ctx_eval(&rt);
    ctx.with(|ctx| {
        let v: f64 = ctx.eval("(function(){ var a=Object.getPrototypeOf(function*(){}); var b=Object.getPrototypeOf(async function(){}); var c=Object.getPrototypeOf(async function*(){}); return 1; })()").unwrap();
        println!("v5_gen_protos {v}");
    });
    drop(ctx);
    drop(rt);
    println!("v5_gen_protos dropped");
}

#[test]
fn v6_gen_ctor_replace() {
    let rt = Runtime::new().unwrap();
    let ctx = ctx_eval(&rt);
    ctx.with(|ctx| {
        let v: f64 = ctx.eval("(function(){ var a=Object.getPrototypeOf(function*(){}); Object.defineProperty(a,\"constructor\",{value:function(){},writable:false,enumerable:false,configurable:false}); return 1; })()").unwrap();
        println!("v6_gen_ctor_replace {v}");
    });
    drop(ctx);
    drop(rt);
    println!("v6_gen_ctor_replace dropped");
}

#[test]
fn v7_async_ctor_replace() {
    let rt = Runtime::new().unwrap();
    let ctx = ctx_eval(&rt);
    ctx.with(|ctx| {
        let v: f64 = ctx.eval("(function(){ var b=Object.getPrototypeOf(async function(){}); Object.defineProperty(b,\"constructor\",{value:function(){},writable:false,enumerable:false,configurable:false}); return 1; })()").unwrap();
        println!("v7_async_ctor_replace {v}");
    });
    drop(ctx);
    drop(rt);
    println!("v7_async_ctor_replace dropped");
}
