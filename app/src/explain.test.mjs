// Synthetic UI/API tests. No real provider, CLI authentication or inference.
import assert from 'node:assert/strict';
import { beforeEach, afterEach, test } from 'node:test';
import { installDom } from '../test/dom.mjs';
import { createRequire } from 'node:module';
import { sessionView, page, queueView, findings } from '../test/fixtures.mjs';
installDom();
globalThis.IS_REACT_ACT_ENVIRONMENT = true;
globalThis.Event = window.Event;
globalThis.CustomEvent = window.CustomEvent;
const require = createRequire(import.meta.url);
const React = require('react');
const { act } = React;
const { createRoot } = require('react-dom/client');
const { createServer } = await import('vite');
const vite = await createServer({ server: { middlewareMode: true, hmr: false }, appType: 'custom' });
const { default: App } = await vite.ssrLoadModule('/src/App.tsx');
test.after(async () => vite.close());
const { selectionCurrent, sameSelection } = await import('./explain.ts');
const json = (body, status = 200) => new Response(JSON.stringify(body), {status, headers:{'Content-Type':'application/json'}});
const providers = [
  {id:'local',label:'Local Ollama',model:'test-model',processing:'local',available:true,detail:'Loopback only; no fallback.'},
  {id:'claude',label:'Claude Code',model:'sonnet',processing:'hosted',available:true,detail:'Sends selected context to your provider.'},
  {id:'codex',label:'Codex',model:'',processing:'hosted',available:false,detail:'No verified tool-free interface.'},
];
let app, calls, view, sources, responder;
class FakeEvents {
  constructor() { this.listeners={}; sources.push(this); }
  addEventListener(name, fn) { (this.listeners[name] ??= []).push(fn); }
  emit(name) { for(const fn of this.listeners[name] ?? []) fn(); }
  close() {}
}
beforeEach(() => {
  document.body.innerHTML=''; app=createRoot(document.body); calls=[]; sources=[]; view=sessionView();
  // Include a real before/after pair in the visual fixture.
  view.files[0].hunks[0].lines.splice(1,0,{id:'removed',kind:'deletion',oldLine:2,newLine:null,text:'    allow_all();'});
  responder = (request) => json({...request,answer:'Old line 2 removed allow_all(); new line 2 adds verify_token(). Red means removed, not an error.'});
  globalThis.EventSource=FakeEvents;
  globalThis.fetch=async (url, init={}) => {
    const path=String(url), method=init.method ?? 'GET';
    const body=init.body ? JSON.parse(init.body) : null;
    calls.push({path,method,body,signal:init.signal});
    if(path==='/api/v1/session') return json(view);
    if(path==='/api/v1/analysis/queue') return json(queueView());
    if(path.startsWith('/api/v1/analysis/findings')) return json(page(findings));
    if(path==='/api/v1/explain/providers') return json({providers});
    if(path==='/api/v1/explain/questions') return responder(body,init.signal);
    if(path.endsWith('/cancel')) return json({cancelled:true});
    throw new Error(`Unexpected request ${method} ${path}`);
  };
});
afterEach(async () => {if(app) await act(async () => app.unmount());});
async function settle() {await act(async () => {await new Promise(r=>setTimeout(r,0));});}
async function start() {await act(async () => app.render(React.createElement(App)));await settle();}
async function click(node) {assert.ok(node);await act(async () => node.click());await settle();}
const button = (name, root=document) => [...root.querySelectorAll('button')].find(b=>b.textContent.trim()===name || b.getAttribute('aria-label')===name);
const panel = () => document.querySelector('[role=dialog][aria-label="Explain this change"]');
const asks = () => calls.filter(c=>c.path==='/api/v1/explain/questions');
async function open() {await click(document.querySelector('[aria-controls="workspace-files"]'));await click(button('Explain removed old line 2'));}
async function explain() {await click(button('Explain this change',panel()));}
async function input(question) {const field=document.getElementById('explain-question');await act(async () => {Object.getOwnPropertyDescriptor(window.HTMLTextAreaElement.prototype,'value').set.call(field,question);field.dispatchEvent(new window.Event('input',{bubbles:true}));});}

test('opening and suggestions never send code; explicit request binds selection; follow-ups keep context',async()=>{
  await start();assert.equal(calls.filter(c=>c.path.includes('/explain/')).length,0);
  await open();assert.equal(asks().length,0);
  assert.match(panel().textContent,/Local processing/);
  assert.match(panel().textContent,/Removed old line 2 and adjacent changes/);
  await click(button('What does this red highlight mean?',panel()));assert.equal(asks().length,0);
  await explain();
  assert.equal(asks().length,1);
  const first=asks()[0].body;
  assert.deepEqual(first.selection,{sessionId:'s1',baseSha:'def',headSha:'abc',fileId:'f-auth',diffDigest:'digest-f-auth',hunkId:'f-auth-h1',lineId:'removed'});
  assert.equal(first.provider,'local');assert.deepEqual(first.history,[]);
  assert.equal(first.code,undefined,'code is server-derived, never browser-supplied');
  assert.match(panel().textContent,/Red means removed, not an error/);
  await input('What could this affect?');await click(button('Send question',panel()));
  assert.deepEqual(asks()[1].body.selection,first.selection);
  assert.equal(asks()[1].body.history[0].answer,document.querySelector('[data-explanation-answer]').textContent);
  assert.ok(calls.every(c=>c.method==='GET'||c.path.includes('/explain/')),'no review mutations');
});

test('provider changes start an empty conversation without making a hosted request',async()=>{
  await start();await open();await explain();
  await click(document.getElementById('explain-provider'));
  await click([...document.querySelectorAll('[role="option"]')].find(node=>node.textContent.includes('Claude Code')));
  assert.equal(asks().length,1);assert.equal(document.querySelectorAll('[data-explanation-answer]').length,0);
  assert.match(panel().textContent,/Hosted processing/);
  await explain();assert.equal(asks()[1].body.provider,'claude');assert.deepEqual(asks()[1].body.history,[]);
});

test('cancel stops display, calls server cancellation, ignores late answers, and permits retry',async()=>{
  let resolve;responder=request=>new Promise(r=>{resolve=()=>r(json({...request,answer:'late answer'}));});
  await start();await open();await explain();assert.match(panel().textContent,/Preparing an explanation/);
  await click(button('Cancel request',panel()));
  assert.equal(asks()[0].signal.aborted,true);
  assert.ok(calls.some(c=>c.path.endsWith(`/${asks()[0].body.requestId}/cancel`)));
  resolve();await settle();assert.doesNotMatch(panel().textContent,/late answer/);
  assert.equal(button('Explain this change',panel()).disabled,false);
});

test('revision change clears answers and cancels outstanding questions even if file digest is unchanged',async()=>{
  await start();await open();await explain();
  let resolve;responder=request=>new Promise(r=>{resolve=()=>r(json({...request,answer:'wrong revision'}));});
  await input('Follow up');await click(button('Send question',panel()));
  view=structuredClone(view);view.pr.headSha='new-head';await act(async () => sources[0].emit('pr.refreshed'));await settle();
  assert.match(panel().textContent,/no longer current/);
  assert.equal(document.querySelectorAll('[data-explanation-answer]').length,0);
  assert.ok(asks()[1].signal.aborted);
  resolve();await settle();assert.doesNotMatch(panel().textContent,/wrong revision/);
});

test('different file closes Q&A and a new selection does not inherit answers',async()=>{
  view.fileStates[1].scope.ignored = false;
  await start();await open();await explain();
  const fileButton=[...document.querySelectorAll('button')].find(b=>b.getAttribute('aria-label')?.startsWith('Open gen/out.rs'));
  await click(fileButton);assert.equal(panel(),null);
  await click(button('Explain added new line 1'));await explain();
  assert.equal(asks()[1].body.selection.fileId,'f-gen');assert.deepEqual(asks()[1].body.history,[]);
});

test('errors and timeouts leave diff and controls usable; provider HTML is inert text',async()=>{
  responder=()=>json({error:'explanation_timeout',message:'Provider timed out; no fallback was used.'},504);
  await start();await open();await explain();
  assert.match(panel().textContent,/Provider timed out/);assert.equal(button('Explain this change',panel()).disabled,false);
  assert.ok(document.querySelector('#file-review .workspace-code'));
  responder=request=>json({...request,answer:'<img src=x onerror="alert(1)"><script>bad()</script> [run](javascript:bad())'});
  await explain();
  const answer=document.querySelector('[data-explanation-answer]');assert.match(answer.textContent,/<script>/);assert.equal(answer.querySelector('img,script,a'),null);
});

test('closing restores keyboard focus; mismatched response is never shown',async()=>{
  responder=request=>json({...request,selection:{...request.selection,headSha:'wrong'},answer:'unrelated answer'});
  await start();await open();await explain();
  assert.match(panel().textContent,/did not match this selection/);assert.doesNotMatch(panel().textContent,/unrelated answer/);
  await click(button('Close explanation'));
  assert.equal(document.activeElement.getAttribute('aria-label'),'Explain removed old line 2');assert.equal(panel(),null);
});

test('review identity and both revisions participate in current-selection check',()=>{
  const selection={sessionId:'s1',baseSha:'def',headSha:'abc',fileId:'f-auth',diffDigest:'digest-f-auth',hunkId:'f-auth-h1',lineId:'l2'};
  assert.equal(selectionCurrent(selection,view),true);
  assert.equal(sameSelection({}, selection), false);
  assert.equal(sameSelection(null, selection), false);
  assert.equal(sameSelection({...selection}, selection), true);
  for(const key of Object.keys(selection)) assert.equal(selectionCurrent({...selection,[key]:'changed'},view),false,key);
});
