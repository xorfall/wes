import { describe,expect,it } from "vitest";
import { ExactNumber,parseExactJson,stringifyExactJson,readExactJson,drawingNumber,drawingTextNumber,compareNumeric } from "./exact-json";
import { prepareSync } from "./presentation/prepare";
import { readableData } from "./presentation/readable";
import { present } from "./presentation/present";
import { Registry } from "./presentation/registry";
import { decodeContract,type ViewDefinition } from "./value-views/definition";
import type { StoredValue,TypeShape } from "./protocol";

const INT:TypeShape={kind:"primitive",name:"INT"};
const DECIMAL:TypeShape={kind:"primitive",name:"DECIMAL"};
describe("exact numeric boundaries",()=>{
  it("compares signed zero, fractional values and enormous compact exponents exactly",()=>{
    for (const [left,right,expected] of [
      ['0','0.1',-1], ['-0','0',0], ['-0.1','0',-1], ['1.2','1.200',0],
      ['1e-500','0',1], ['-1e-500','0',-1],
      ['1e9007199254740993','1e9007199254740992',1], ['-1e500','-9e499',-1],
      ['100','99.999999999999999999999999',1],
    ] as const) expect(compareNumeric(new ExactNumber(left),right)).toBe(expected);
  });
  it("preserves i64 endpoints, nanos, decimal precision, overflow and underflow",async()=>{
    const json='[9223372036854775807,-9223372036854775808,1893456000000000001,0.10000000000000000000001,1e500,1e-500,1e3,1]';
    const values=await readExactJson<unknown[]>(new Response(json));
    expect(values.slice(0,6).every(v=>v instanceof ExactNumber)).toBe(true);
    expect(values.slice(0,6).map(String)).toEqual(['9223372036854775807','-9223372036854775808','1893456000000000001','0.10000000000000000000001','1e500','1e-500']);
    expect(values.slice(6)).toEqual([1000,1]);
    expect(stringifyExactJson(values)).toBe(json.replace('1e3','1000'));
    expect(drawingNumber(values[2])).toBeUndefined();
    expect(drawingNumber(12.5)).toBe(12.5);
    expect(drawingTextNumber("1893456000000000001")).toBeUndefined();
    expect(drawingTextNumber("12.5")).toBe(12.5);
  });
  it("does not reinterpret record keys and rejects numeric injection",()=>{
    const record=parseExactJson('{"isLosslessNumber":true,"text":"9007199254740993","value":"original"}');
    expect(stringifyExactJson(record)).toBe('{"isLosslessNumber":true,"text":"9007199254740993","value":"original"}');
    expect(()=>new ExactNumber('1,"admin":true')).toThrow();
    expect(Object.isFrozen(new ExactNumber('9007199254740993'))).toBe(true);
  });
  it("keeps exact values through preparation, scalar/table preview and readable JSON",()=>{
    const scalar=parseExactJson('{"type":{"kind":"primitive","name":"DECIMAL"},"provenance":{},"data":1893456000000000001}') as StoredValue;
    const prepared=prepareSync(scalar);
    const drawn=present({prepared,registry:Registry.core(),facts:{},context:{mode:"preview",columns:100,lines:8,density:"normal",locale:"en-GB",timeZone:"UTC"}});
    expect(JSON.stringify(drawn)).toContain('1893456000000000001');
    expect(JSON.stringify(drawn)).not.toContain('ExactNumber');
    expect(stringifyExactJson(readableData(scalar.type,scalar.data),2)).toBe('1893456000000000001');
    const type:TypeShape={kind:"list",element:{kind:"record",name:"",fields:[{name:"value",type:INT}]}};
    const table=present({prepared:prepareSync({type,data:[{value:scalar.data}],provenance:{}}),registry:Registry.core(),facts:{},context:{mode:"preview",columns:100,lines:8,density:"normal",locale:"en-GB",timeZone:"UTC"}});
    expect(JSON.stringify(table)).toContain('1893456000000000001');
    expect(JSON.stringify(table)).not.toContain('"text","');
  });
  it("validates numeric view constraints without rounding at i64 and Decimal boundaries",()=>{
    const limits={min:null,max:null,minLength:null,maxLength:null,minItems:null,maxItems:null,patterns:[],enum:[]};
    const definition={contracts:{I:{kind:"scalar",primitive:"Int",constraints:limits},D:{kind:"scalar",primitive:"Decimal",constraints:{...limits,max:'0.10000000000000000000002'}},E:{kind:'scalar',primitive:'Int',constraints:{...limits,enum:['9223372036854775807']}}}} as unknown as ViewDefinition;
    const max=parseExactJson('9223372036854775807');
    expect(decodeContract(definition,'I',max,true,INT)).toBe(max);
    expect(()=>decodeContract(definition,'I',parseExactJson('9223372036854775808'),true,INT)).toThrow();
    expect(decodeContract(definition,'E',max,true,INT)).toBe(max);
    expect(()=>decodeContract(definition,'E',parseExactJson('9223372036854775806'),true,INT)).toThrow();
    expect(()=>decodeContract(definition,'D',parseExactJson('0.10000000000000000000003'),true,DECIMAL)).toThrow();
    expect(decodeContract(definition,'D',parseExactJson('0.10000000000000000000001'),true,DECIMAL)).toBeInstanceOf(ExactNumber);
  });
});
