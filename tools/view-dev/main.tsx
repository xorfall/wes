import { useState } from "react";
import { createRoot } from "react-dom/client";
import { ValueBlock as ViewHost } from "../../gui/src/surface/render/ValueBlock";
import { PeekScreen } from "../../gui/src/surface/screens/Peek";
import { timelineFixtures } from "./work/timeline";
import { fixtures as staticFixtures } from "./work/fixtures";
import "../../gui/src/surface/tokens.css";
import "../../gui/src/surface/screens.css";
import "../../gui/src/surface/inspection.css";
import "./style.css";

const fixtures = [...staticFixtures.filter(it=>it.id!=="module"), ...timelineFixtures];

function Workbench() {
  const [selected, setSelected] = useState("ok"), [mode, setMode] = useState<"preview" | "expanded">("preview");
  const [peek, setPeek] = useState(false), [narrow, setNarrow] = useState(false);
  const sample = fixtures.find(it => it.id === selected)!;
  return <div className="wes-terminal view-workbench">
    <header><strong>wes · view development</strong><span className="mono-dim">Synthetic values · no backend</span></header>
    <nav aria-label="Design scenarios">{fixtures.map(it => <button key={it.id} aria-pressed={selected === it.id} onClick={() => setSelected(it.id)}>{it.label}</button>)}</nav>
    <div className="view-workbench-controls"><button onClick={() => setMode(it => it === "preview" ? "expanded" : "preview")}>{mode} · toggle</button><label><input type="checkbox" checked={narrow} onChange={e => setNarrow(e.target.checked)} /> Narrow pane</label><button onClick={() => setPeek(true)}>Open /peek</button></div>
    <main style={{ maxWidth: narrow ? 420 : 1000 }}>
      <p><span className="mono-ok">✓</span> <span className="mono-ref">$response</span> <span className="mono-dim">· stored synthetic result</span></p>
      <ViewHost key={sample.id} value={sample.value} cacheKey={`view-dev:${sample.id}`} mode={mode} inCell={false} />
    </main>
    {peek && <div className="view-workbench-peek"><PeekScreen top={[]} subject={[{ text: "$response", role: "mono-ref" }]} what="value" value={sample.value} onClose={() => setPeek(false)} /></div>}
  </div>;
}
createRoot(document.getElementById("root")!).render(<Workbench />);
