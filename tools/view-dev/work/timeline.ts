import type { StoredValue, TypeShape } from "../../../gui/src/protocol";
import { instant, nanos } from "../../../gui/src/value-views/temporal";
const p = (name: string): TypeShape => ({ kind: "primitive", name });
const record = (name: string, fields: Record<string, TypeShape>): TypeShape => ({ kind: "record", name, fields: Object.entries(fields).map(([name, type]) => ({ name, type })) });
const list = (element: TypeShape): TypeShape => ({ kind: "list", element });
export const timelineType = record("Timeline", {
  view: p("TEXT"), id: p("TEXT"), title: p("TEXT"), range: p("INTERVAL"), coverage: p("INTERVAL"), omitted: p("INT"), sourceError: p("TEXT"),
  series: list(record("MetricSeries", { id: p("TEXT"), label: p("TEXT"), unit: p("TEXT"), samples: list(record("Sample", { id: p("TEXT"), at: p("INSTANT"), value: p("DECIMAL"), gap: p("BOOL") })) })),
  events: list(record("TimelineEvent", { id: p("TEXT"), at: p("INSTANT"), label: p("TEXT"), detail: p("TEXT") })),
});
export const groupType = record("TimelineGroup", { view: p("TEXT"), title: p("TEXT"), range: p("INTERVAL") });
const from = "2030-06-15T08:00:00Z", to = "2030-06-15T08:10:00Z";
export const demoRange = `${from}/${to}`;
export function timelineData(id = "requests", count = 60) {
  const start = nanos(from)!;
  return { view: "timeline", id, title: id === "requests" ? "Request rate" : id, range: demoRange, coverage: demoRange, omitted: 0, sourceError: "",
    series: [{ id: "rate", label: "requests", unit: "/s", samples: Array.from({ length: count }, (_, i) => ({ id: `sample-${i}`, at: instant(start + BigInt(i) * 600_000_000_000n / BigInt(Math.max(1, count))), value: 20 + (i % 9) * 3 + (i % 17 === 0 ? 80 : 0), gap: i > count * .4 && i < count * .5 })) }],
    events: [{ id: "release", at: "2030-06-15T08:03:15.000000007Z", label: "Release installed", detail: "Synthetic worker revision changed." }],
  };
}
export const timelineValue = (id = "requests", count = 60): StoredValue => ({ type: timelineType, data: timelineData(id, count), provenance: {} });
export const groupValue = (_count = 0): StoredValue => ({type:groupType,provenance:{},data:{view:"timeline-group",title:"Connect Timeline instances to this group",range:demoRange}});
export const timelineFixtures = [
  { id: "timeline", label: "Timeline", value: timelineValue() },
  { id: "timeline-group", label: "Timeline group · unconnected", value: groupValue() },
  {id:"metric",label:"Metric",value:{type:record("Metric",{view:p("TEXT"),value:p("DECIMAL"),label:p("TEXT"),unit:p("TEXT"),status:p("TEXT")}),data:{view:"metric",value:125.5,label:"Latency",unit:"ms",status:"healthy"},provenance:{}}},
  {id:"histogram",label:"Histogram",value:{type:record("Histogram",{view:p("TEXT"),total:p("INT"),bins:list(record("HistogramBin",{lower:p("DECIMAL"),upper:p("DECIMAL"),count:p("INT")}))}),data:{view:"histogram",total:4,bins:[{lower:0,upper:10,count:3},{lower:10,upper:20,count:1}]},provenance:{}}},
  {id:"choice",label:"Choice",value:{type:record("Choice",{view:p("TEXT"),title:p("TEXT"),options:list(record("ChoiceOption",{value:p("TEXT"),label:p("TEXT")}))}),data:{view:"choice",title:"Feed",options:[{value:"demo",label:"Synthetic requests"},{value:"empty",label:"Empty"}]},provenance:{}}},
  {id:"dashboard",label:"Dashboard · unconnected",value:{type:record("Dashboard",{view:p("TEXT"),title:p("TEXT")}),data:{view:"dashboard",title:"Connect View instances to this dashboard"},provenance:{}}},
];
