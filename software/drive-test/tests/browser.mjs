// SPDX-License-Identifier: GPL-3.0-only
import {spawn} from 'node:child_process';
import {writeFile, mkdtemp, rm} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import assert from 'node:assert/strict';
const delay = ms => new Promise(r => setTimeout(r,ms));
async function waitFor(check, message, ms=7000) {
  const until = Date.now()+ms;
  while(Date.now()<until) { if(await check()) return; await delay(50); }
  throw new Error(message);
}
const children=[];
const launch=(cmd,args,options={}) => { const p=spawn(cmd,args,{stdio:['ignore','pipe','pipe'],...options}); children.push(p); return p; };
let debuggerSocket;
const profile = await mkdtemp(join(tmpdir(), 'cuttlefish-browser-'));
try {
  const base = process.argv[2];
  if (!base) throw new Error('Pass the HTTP base URL supplied by the Rust smoke test');
  const chrome=launch(process.env.CHROME_BIN || (process.platform === 'darwin' ? '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome' : 'chromium'),
    ['--headless=new','--disable-gpu','--no-first-run','--no-default-browser-check',
     '--remote-debugging-port=0','--remote-debugging-address=127.0.0.1',`--user-data-dir=${profile}`,'about:blank']);
  let chromeLog=''; chrome.stderr.on('data',d=>{chromeLog+=d;});
  chrome.on('error',error=>{chromeLog+=error.message;});
  let target;
  try {
    await waitFor(async()=>{
      const address = chromeLog.match(/DevTools listening on (ws:\/\/[^/]+)/)?.[1];
      if (!address) return false;
      try { target=(await(await fetch(address.replace('ws:', 'http:')+'/json')).json()).find(t=>t.type==='page'); return !!target; } catch { return false; }
    },'Chrome debugging endpoint unavailable',25000);
  } catch (error) { throw new Error(error.message+'\n'+chromeLog); }
  debuggerSocket=new WebSocket(target.webSocketDebuggerUrl);
  await new Promise((r,j)=>{debuggerSocket.onopen=r;debuggerSocket.onerror=j;});
  let next=0; const pending=new Map(); const errors=[];
  debuggerSocket.onmessage=event=>{
    const m=JSON.parse(event.data);
    if(m.id){const p=pending.get(m.id);pending.delete(m.id);m.error?p.reject(m.error):p.resolve(m.result);}
    else if(m.method==='Runtime.exceptionThrown')errors.push(m.params.exceptionDetails.text);
  };
  const call=(method,params={})=>new Promise((resolve,reject)=>{const id=++next;pending.set(id,{resolve,reject});debuggerSocket.send(JSON.stringify({id,method,params}));});
  const evaluate=async(expression)=>{const r=await call('Runtime.evaluate',{expression,returnByValue:true,awaitPromise:true}); if(r.exceptionDetails)throw new Error(JSON.stringify(r.exceptionDetails));return r.result.value;};
  await call('Runtime.enable'); await call('Page.enable');
  await call('Emulation.setDeviceMetricsOverride',{width:1280,height:900,deviceScaleFactor:1,mobile:false});
  await call('Page.navigate',{url:`${base}/#token=test-control-token`});
  await call('Page.bringToFront');
  await waitFor(()=>evaluate("document.readyState === 'complete' && !document.getElementById('arm').disabled"),'Arm button not enabled');
  assert.equal(await evaluate("document.hasFocus()"),true);
  await evaluate("document.getElementById('arm').click()");
  await waitFor(()=>evaluate('active'),'Control not acquired');
  await evaluate("window.dispatchEvent(new KeyboardEvent('keydown',{code:'KeyW'}));window.dispatchEvent(new KeyboardEvent('keydown',{code:'KeyA'}))");
  await delay(120);
  assert.equal(await evaluate('active && axes().throttle === 80 && axes().steering === -100'),true);
  await evaluate("window.dispatchEvent(new KeyboardEvent('keyup',{code:'KeyA'}));window.dispatchEvent(new KeyboardEvent('keyup',{code:'KeyW'}))");
  await delay(120);
  assert.equal(await evaluate('axes().throttle === 0 && axes().steering === 0'),true);
  await evaluate("window.dispatchEvent(new KeyboardEvent('keydown',{code:'ArrowUp'}));window.dispatchEvent(new KeyboardEvent('keydown',{code:'ArrowRight'}))");
  await delay(120);
  assert.equal(await evaluate('active && axes().throttle === 80 && axes().steering === 100'),true);
  await evaluate("window.dispatchEvent(new KeyboardEvent('keydown',{code:'Space'}))");
  await delay(100);
  assert.equal(await evaluate('active'),false);
  await delay(300);
  await evaluate("document.getElementById('arm').click()");
  await waitFor(()=>evaluate('active'),'Rearming failed');
  await delay(100);
  await evaluate("window.dispatchEvent(new Event('blur'))");
  await delay(100);
  assert.equal(await evaluate('active'),false);
  assert.equal(await evaluate("document.documentElement.scrollWidth <= innerWidth"),true);
  const shot=await call('Page.captureScreenshot',{format:'png'});
  await writeFile(join(tmpdir(),'cuttlefish-web-desktop.png'),Buffer.from(shot.data,'base64'));
  await call('Emulation.setDeviceMetricsOverride',{width:390,height:844,deviceScaleFactor:1,mobile:true});
  await delay(100);
  assert.equal(await evaluate("document.documentElement.scrollWidth <= innerWidth"),true);
  const mobile=await call('Page.captureScreenshot',{format:'png',captureBeyondViewport:true});
  await writeFile(join(tmpdir(),'cuttlefish-web-mobile.png'),Buffer.from(mobile.data,'base64'));
  assert.deepEqual(errors,[]);
  console.log('Browser checks passed: real Rust HTTP/WebSocket server, WASD/arrows, release, Space, focus loss, rearm, desktop/mobile layout, no JS errors.');
} finally {
  debuggerSocket?.close();
  for(const child of children.reverse()) {
    const closed = new Promise(resolve=>child.once('exit',resolve));
    if (child.exitCode === null && child.signalCode === null) {
      child.kill('SIGTERM');
      await Promise.race([closed,delay(2000)]);
      if(child.exitCode === null && child.signalCode === null)child.kill('SIGKILL');
    }
  }
  await rm(profile,{recursive:true,force:true});
}
