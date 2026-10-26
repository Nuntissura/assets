import assert from "node:assert/strict";
import {readFile} from "node:fs/promises";
import {pathToFileURL} from "node:url";
import {resolve,dirname,join} from "node:path";

const gluePath=resolve(process.argv[2]),glue=await import(pathToFileURL(gluePath).href);
glue.initSync({module:await readFile(join(dirname(gluePath),"handpick_bg.wasm"))});
assert.equal(typeof glue.matchLiteral,"function");
const fixture=JSON.parse(await readFile(new URL("./fixtures/literal-matching-v1.json",import.meta.url),"utf8"));
for(const row of fixture.cases){const {match,...input}=row;assert.deepEqual(JSON.parse(glue.matchLiteral(JSON.stringify({version:fixture.version,...input}))),{version:1,match});}
for(const [input,error] of [
  [{version:2,needle:"x",candidate:"x",max_edits:0},"InvalidWire"],
  [{version:1,needle:"x",candidate:"x",max_edits:0,unexpected:true},"InvalidWire"],
  [{version:1,needle:"x",candidate:"x",max_edits:-1},"InvalidWire"],
  [{version:1,needle:"",candidate:"x",max_edits:0},"EmptyNeedle"],
  [{version:1,needle:"x",candidate:"x",max_edits:9},"ThresholdLimit"],
  [{version:1,needle:"😀".repeat(65),candidate:"x",max_edits:1},"NeedleLimit"],
  [{version:1,needle:"x",candidate:"猫".repeat(257),max_edits:1},"CandidateLimit"],
])assert.throws(()=>glue.matchLiteral(JSON.stringify(input)),failure=>String(failure)===error);
assert.throws(()=>glue.matchLiteral(" ".repeat(65537)),failure=>String(failure)==="PayloadLimit");
const exact="😀".repeat(64);assert.deepEqual(JSON.parse(glue.matchLiteral(JSON.stringify({version:1,needle:exact,candidate:exact,max_edits:0}))),{version:1,match:{edit_distance:0}});
console.log(JSON.stringify({status:"pass",scope:"actual compiled optional literal matcher WASM",vectors:fixture.cases.length,negative_boundaries:8}));
