/** Real PTYs + a six-slot HTTP/1 client, using the GUI's actual input coalescer.
 * Node >=22 and gui/node_modules are prerequisites. Only temporary synthetic data.
 */
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import http from 'node:http';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';
const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..');
const require = createRequire(path.join(root, 'gui/package.json'));
const ts = require('typescript');
const code = ts.transpileModule(fs.readFileSync(path.join(root, 'gui/src/terminal-input.ts'), 'utf8'),
  {compilerOptions: {target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ES2022}}).outputText;
const { terminalInput } = await import('data:text/javascript;base64,' + Buffer.from(code).toString('base64'));
const binary = path.resolve(process.argv[2] ?? path.join(root, 'target/debug/wes'));
const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));
const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'wes-input-contention-'));
fs.writeFileSync(path.join(directory, 'echo.py'), 'import os, tty\ntty.setraw(0)\nos.write(1,b"READY\\n")\nwhile True:\n data=os.read(0,8192)\n if not data: break\n os.write(1,data)\n');
const server = spawn(binary, ['--home', path.join(directory, 'home'), '--serve', '0'], {cwd:directory, stdio:['ignore','pipe','pipe']});
let stderr = ''; server.stderr.on('data', bytes => { stderr += bytes; });
const agent = new http.Agent({keepAlive:true, maxSockets:6});
const retained = new Set();
let base, generation;
async function request(route, body) {
  return new Promise((resolve,reject) => {
    const payload = body === undefined ? undefined : JSON.stringify(body);
    const req = http.request(base + route, {agent, method:payload ? 'POST':'GET', headers: {
      ...(payload ? {'Content-Type':'application/json','Content-Length':Buffer.byteLength(payload)} : {}),
      ...(generation ? {'X-Wes-Session':generation} : {}),
    }}, response => {
      const chunks=[];response.on('data',b=>chunks.push(b));response.on('error',reject);
      response.on('end',()=> {
        const text=Buffer.concat(chunks).toString();
        if (response.statusCode >= 400) reject(new Error(`${route}: ${response.statusCode} ${text}`));
        else { try {resolve(JSON.parse(text));} catch(error) {reject(error);} }
      });
    });
    req.setTimeout(8000,()=>req.destroy(new Error('request deadline')));
    req.on('error',reject);req.end(payload);
  });
}
function stream(route, websocket) {
  return new Promise((resolve,reject) => {
    if (websocket) {
      const ws=new WebSocket(base.replace('http:', 'ws:') + route.replace('/events','/events/socket'));
      retained.add(ws);
      ws.addEventListener('error',()=>reject(new Error('socket failed')));
      ws.addEventListener('message',event=> {
        const batch=JSON.parse(event.data);
        assert(Number.isSafeInteger(batch.sequence) && Array.isArray(batch.events));
        ws.send(`ack:${batch.sequence}`);
        const session=batch.events.find(value=>value.event==='session');
        if(session) resolve(session.generation);
      });
    } else {
      const req=http.get(base+route,{agent},response=> {
        assert.equal(response.statusCode,200);
        let pending=''; response.on('data',bytes=> {
          pending+=bytes.toString();let newline;
          while((newline=pending.indexOf('\n'))>=0) {
            const line=pending.slice(0,newline);pending=pending.slice(newline+1);
            if(line.startsWith('data:')) { const value=JSON.parse(line.slice(5)); if(value.event==='session')resolve(value.generation); }
          }
        });
      });
      retained.add(req);req.on('error',reject);
    }
  });
}
async function streamsClose() {
  for(const connection of retained) {
    if(connection instanceof WebSocket) connection.close(); else connection.destroy();
  }
  retained.clear(); await sleep(50);
}
const action = (name,values={})=>request('/terminals',{client:'synthetic-contention',action:name,...values});
async function measure(websocket) {
  // Origin controller plus three retained workspace controllers, as in a tabbed GUI.
  generation=await stream('/events',websocket);
  for(const name of ['one','two','three'])await stream('/events?workspace='+name,websocket);
  const terminals=[];
  let stopped=false;
  const readers=[];
  try {
    for(let i=0;i<2;i++) {
      const {id}=await action('start');
      const term={id,cursor:0,received:'',arrivals:[]};terminals.push(term);
      await action('write',{id,text:'python3 echo.py\r'});
      let output='';
      while(!output.includes('READY\n')) {
        const frame=await action('poll',{id,cursor:term.cursor,wait_ms:1000});
        term.cursor=frame.next;output+=Buffer.from(frame.data,'base64').toString();
      }
    }
    for(const term of terminals) readers.push((async()=> {
      while(!stopped) {
        const frame=await action('poll',{id:term.id,cursor:term.cursor,wait_ms:1000});
        assert.equal(frame.start,term.cursor);term.cursor=frame.next;
        const text=Buffer.from(frame.data,'base64').toString();
        term.received+=text;
        for(const char of text) term.arrivals.push(performance.now());
      }
    })());
    await sleep(80); // Both empty readers now hold their HTTP slots.
    const errors=[];
    const input=terminalInput(text=>action('write',{id:terminals[0].id,text}),error=>errors.push(error));
    const pressed=[];
    let expected='';
    for(const [count,cadence,char] of [[20,40,'a'],[60,5,'b']]) {
      for(let i=0;i<count;i++) {pressed.push(performance.now());expected+=char;input.push(char);await sleep(cadence);}
    }
    const deadline=performance.now()+8000;
    while(terminals[0].received.length<expected.length && !errors.length && performance.now()<deadline)await sleep(10);
    input.dispose();assert.deepEqual(errors,[]);assert.equal(terminals[0].received,expected);
    const latency=pressed.map((time,i)=>terminals[0].arrivals[i]-time).sort((a,b)=>a-b);
    return {transport:websocket?'websocket':'sse',keys:expected.length,median_ms:+latency[Math.floor(latency.length/2)].toFixed(2),p95_ms:+latency[Math.floor(latency.length*.95)].toFixed(2),max_ms:+latency.at(-1).toFixed(2)};
  } finally {
    stopped=true;await streamsClose();await Promise.all(readers);
    for(const term of terminals)await action('close',{id:term.id});
  }
}
try {
  base=await new Promise((resolve,reject)=> {
    let output='';const timer=setTimeout(()=>reject(new Error('startup deadline')),20000);
    server.stdout.on('data',bytes=> {output+=bytes.toString();const match=output.match(/Listening at (http:\/\/[^\s]+)/);if(match){clearTimeout(timer);resolve(match[1]);}});
    server.once('exit',code=>{clearTimeout(timer);reject(new Error(`startup ${code}: ${stderr}`));});
  });
  for(const name of ['one','two','three'])await request('/workspaces',{name,create:true});
  const before=await measure(false);
  const after=await measure(true);
  console.log(JSON.stringify({before,after}));
  assert(before.p95_ms>500,'fixture did not reproduce HTTP slot starvation');
  assert(after.p95_ms<250,'typing still waits behind idle readers');
  assert(after.p95_ms*3<before.p95_ms,'insufficient contention improvement');
  console.log('PASS: two real PTYs, four retained workspace streams, paced typing/key repeat, exact input order and bounded echo latency');
} finally {
  await streamsClose();agent.destroy();
  server.kill('SIGINT');
  const timeout=setTimeout(()=>server.kill('SIGKILL'),10000);
  await once(server,'exit');clearTimeout(timeout);
  fs.rmSync(directory,{recursive:true,force:true});
}
