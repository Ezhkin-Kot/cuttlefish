// SPDX-License-Identifier: GPL-3.0-only
'use strict';
const $ = (id) => document.getElementById(id);
const keyMap = {KeyW:'up', ArrowUp:'up', KeyS:'down', ArrowDown:'down',
  KeyA:'left', ArrowLeft:'left', KeyD:'right', ArrowRight:'right'};
const keys = new Set();
const pointers = new Map();
let token = new URLSearchParams(location.hash.slice(1)).get('token') || sessionStorage.getItem('cuttlefish-token') || '';
let controlSocket = null, videoSocket = null;
let active = false, connecting = false, paused = false, pauseReason = '', serverStatus = null;
let lastRendered = 0, lastSample = 0, videoGeneration = 0, pendingFrame = null, decoding = false;
let frameCount = 0, byteCount = 0, statsAt = performance.now();
let stoppedReason = '';
let immersive = false, nativeFullscreen = false, fullscreenChanging = false, requestedFullscreenExit = false;

function url(path, websocket = false) {
  const result = new URL(path, location.href);
  if (websocket) result.protocol = location.protocol === 'https:' ? 'wss:' : 'ws:';
  result.searchParams.set('token', token);
  return result.toString();
}
function stopUrl(reason) {
  const result = new URL(url('/api/stop'));
  result.searchParams.set('reason', reason);
  return result.toString();
}
function clearInput() { keys.clear(); pointers.clear(); renderInput(); }
function hasFreshVideo() {
  return videoSocket?.readyState === WebSocket.OPEN && lastRendered > 0 && performance.now() - lastRendered < 700;
}
function axes() {
  const held = new Set([...keys].map(code => keyMap[code]));
  for (const key of pointers.values()) held.add(key);
  return {
    throttle: active && !paused ? (Number(held.has('up')) - Number(held.has('down'))) * Number($('throttle').value) : 0,
    steering: active && !paused ? (Number(held.has('right')) - Number(held.has('left'))) * 100 : 0,
    held
  };
}
function renderInput() {
  const state = axes();
  $('throttle-value').textContent = state.throttle;
  $('steering-value').textContent = state.steering;
  document.querySelectorAll('[data-key]').forEach(button => {
    button.dataset.held = active && state.held.has(button.dataset.key);
    button.disabled = !active || paused;
  });
}
function renderStatus() {
  const ready = serverStatus?.serial_ready && serverStatus?.camera_ready;
  const freshVideo = serverStatus && (!serverStatus.camera_required || hasFreshVideo());
  $('arm').disabled = !ready || !freshVideo || active || connecting || serverStatus?.controlling;
  $('stop').disabled = !token;
  $('arm').textContent = active ? 'You have control' : connecting ? 'Connecting…' : 'Take control';
  $('status').classList.toggle('active', active);
  $('status').textContent = active ? (paused ? `Paused: ${pauseReason}. Release the keys and wait` : 'Ready to drive. Hold the direction keys') : stoppedReason || serverStatus?.reason || 'Waiting for server';
  $('connection').textContent = serverStatus ? (serverStatus.serial_ready ? 'Arduino connected' : 'Arduino unavailable') : 'Disconnected';
  $('telemetry').textContent = serverStatus?.mcu || 'Waiting for Arduino state';
  renderInput();
}
function stopControl(reason = 'Vehicle stopped', notify = true) {
  console.info(`[cuttlefish] Stop: ${reason}`);
  stoppedReason = reason;
  const socket = controlSocket;
  controlSocket = null;
  active = false;
  connecting = false;
  paused = false;
  pauseReason = '';
  clearInput();
  if (socket) {
    if (socket.readyState === WebSocket.OPEN) socket.send(JSON.stringify({type:'stop', reason}));
    socket.close();
  }
  if (notify && token) {
    fetch(stopUrl(reason), {method:'POST', keepalive:true}).catch(() => {});
  }
  renderStatus();
}
// A transient delay stops motion without abandoning a healthy control socket.
// Clear held inputs once; key autorepeat cannot restore them after recovery.
function pauseControl(reason, notify = true) {
  if (!active && !connecting) return;
  if (!paused) {
    paused = true;
    clearInput();
    if (notify && controlSocket?.readyState === WebSocket.OPEN) {
      controlSocket.send(JSON.stringify({type:'pause'}));
    }
  }
  pauseReason = reason;
  renderStatus();
}
function acquire() {
  if (active || connecting || document.hidden || !document.hasFocus()) return;
  connecting = true;
  stoppedReason = '';
  clearInput();
  const socket = new WebSocket(url('/control', true));
  controlSocket = socket;
  renderStatus();
  socket.onmessage = (event) => {
    if (controlSocket !== socket) return;
    try {
      const message = JSON.parse(event.data);
      if (message.type === 'error') { stopControl(message.message, false); return; }
      if (message.type !== 'sample' || !Number.isSafeInteger(message.ticket)) throw new Error('Unexpected server response');
      if (document.hidden || !document.hasFocus()) { stopControl('Page lost focus'); return; }
      serverStatus = message.status;
      if (serverStatus.camera_required && !hasFreshVideo()) {
        pauseControl('Waiting for fresh video');
      } else if (serverStatus.paused) {
        pauseControl(serverStatus.reason, false);
      } else if (paused) {
        paused = false;
        pauseReason = '';
        clearInput();
      }
      connecting = false;
      active = true;
      lastSample = performance.now();
      const {throttle, steering} = axes();
      // Reply only to the current server ticket; no timer repeats stale throttle.
      socket.send(JSON.stringify({type:'sample', ticket:message.ticket, throttle, steering}));
      renderStatus();
    } catch (error) { stopControl(`Control error: ${error.message}`); }
  };
  socket.onclose = (event) => {
    if (controlSocket === socket) stopControl(`Control connection closed (${event.code}). Take control again`, false);
  };
  socket.onerror = () => {
    if (controlSocket === socket) stopControl('Control server connection lost', false);
  };
}

// Reuse the existing canvas and controls so view changes cannot create a
// second control connection or a second set of held keys.
function setImmersive(enabled) {
  immersive = enabled;
  $('drive-view').classList.toggle('immersive', enabled);
  document.body.classList.toggle('view-open', enabled);
  $('fullscreen').textContent = enabled ? 'Exit full screen' : 'Full screen ⛶';
  $('fullscreen').setAttribute('aria-pressed', String(enabled));
  $('fullscreen').setAttribute('aria-label', enabled ? 'Exit full screen' : 'Enter full screen');
  $('fullscreen').title = enabled ? 'Exit full screen (F)' : 'Full screen (F)';
  if (!enabled) $('view-notice').hidden = true;
}

async function leaveFullscreen() {
  if (fullscreenChanging || !immersive) return;
  if (active || connecting) pauseControl('Switching view');
  if (document.fullscreenElement === $('drive-view')) {
    fullscreenChanging = true;
    requestedFullscreenExit = true;
    $('fullscreen').disabled = true;
    try {
      await document.exitFullscreen();
    } catch {
      requestedFullscreenExit = false;
      $('view-notice').textContent = 'Use Esc to leave full screen';
      $('view-notice').hidden = false;
    } finally {
      fullscreenChanging = false;
      $('fullscreen').disabled = false;
    }
  } else {
    setImmersive(false);
  }
  $('fullscreen').focus({preventScroll:true});
}

async function toggleFullscreen() {
  if (fullscreenChanging) return;
  if (immersive) { await leaveFullscreen(); return; }
  if (active || connecting) pauseControl('Switching view');
  setImmersive(true);
  $('fullscreen').focus({preventScroll:true});
  fullscreenChanging = true;
  $('fullscreen').disabled = true;
  try {
    if (!$('drive-view').requestFullscreen || !document.fullscreenEnabled) {
      throw new Error('Fullscreen unavailable');
    }
    await $('drive-view').requestFullscreen();
  } catch {
    // Keep a useful edge-to-edge view if native fullscreen is unavailable or
    // denied, rather than leaving the camera and controls inaccessible.
    if (immersive) {
      $('view-notice').textContent = 'Expanded view · F to exit';
      $('view-notice').hidden = false;
    }
  } finally {
    fullscreenChanging = false;
    $('fullscreen').disabled = false;
  }
}

$('fullscreen').addEventListener('click', toggleFullscreen);
document.addEventListener('fullscreenchange', () => {
  if (document.fullscreenElement === $('drive-view')) {
    nativeFullscreen = true;
    setImmersive(true);
    $('view-notice').hidden = true;
  } else if (nativeFullscreen) {
    nativeFullscreen = false;
    const requested = requestedFullscreenExit;
    requestedFullscreenExit = false;
    setImmersive(false);
    // Some browsers consume Escape before dispatching keydown to the page.
    // Preserve the existing Escape-stop behavior through fullscreenchange.
    if (!requested && (active || connecting)) stopControl('Full screen exited by browser');
  }
});

async function decodeFrames(generation) {
  if (decoding) return;
  decoding = true;
  try {
    while (pendingFrame && generation === videoGeneration) {
      const blob = pendingFrame;
      pendingFrame = null;
      const bitmap = await createImageBitmap(blob);
      if (generation !== videoGeneration) { bitmap.close(); return; }
      const canvas = $('camera');
      if (canvas.width !== bitmap.width || canvas.height !== bitmap.height) {
        canvas.width = bitmap.width; canvas.height = bitmap.height;
      }
      canvas.getContext('2d').drawImage(bitmap, 0, 0);
      bitmap.close();
      lastRendered = performance.now();
      $('video-message').hidden = true;
      frameCount++; byteCount += blob.size;
      const elapsed = (lastRendered - statsAt) / 1000;
      if (elapsed >= 1) {
        $('video-stats').textContent = `${canvas.width}×${canvas.height} · ${(frameCount/elapsed).toFixed(1)} FPS · ${(byteCount*8/elapsed/1e6).toFixed(1)} Mbit/s`;
        $('view-stats').textContent = $('video-stats').textContent;
        frameCount = 0; byteCount = 0; statsAt = lastRendered;
      }
    }
  } catch (error) {
    pauseControl(`Could not display video: ${error.message}`);
  } finally {
    decoding = false;
    if (pendingFrame) decodeFrames(videoGeneration);
  }
}
function connectVideo() {
  if (!token) return;
  const generation = ++videoGeneration;
  const socket = new WebSocket(url('/ws', true));
  videoSocket = socket;
  socket.binaryType = 'blob';
  socket.onmessage = (event) => {
    if (videoSocket !== socket) return;
    pendingFrame = event.data; // one latest JPEG; no queue of decodes
    decodeFrames(generation);
  };
  socket.onclose = () => {
    if (videoSocket !== socket) return;
    $('video-message').hidden = false;
    $('video-message').textContent = 'Video disconnected. Reconnecting…';
    if (active && serverStatus?.camera_required) pauseControl('Video reconnecting');
    setTimeout(() => { if (videoSocket === socket) connectVideo(); }, 1000);
  };
}
async function pollStatus() {
  if (!token || active || connecting) return;
  const requestToken = token;
  try {
    const response = await fetch(url('/api/status'), {cache:'no-store', signal:AbortSignal.timeout(1500)});
    if (!response.ok) throw new Error(response.status === 401 ? 'Invalid access token' : 'Server unavailable');
    const status = await response.json();
    if (token !== requestToken || active || connecting) return;
    serverStatus = status;
  } catch (error) {
    // An in-flight status poll may fail after control was acquired. Control
    // samples already carry authoritative status; do not tear down that socket.
    if (token !== requestToken || active || connecting) return;
    serverStatus = null;
    stopControl(error.message, false);
  }
  renderStatus();
}
function authenticate(value) {
  if (!value) return;
  if (controlSocket) stopControl('Access token changed');
  token = value;
  sessionStorage.setItem('cuttlefish-token', token);
  history.replaceState(null, '', location.pathname + location.search);
  $('token').value = token;
  stoppedReason = '';
  if (videoSocket) { const old = videoSocket; videoSocket = null; old.close(); }
  connectVideo();
  pollStatus();
}
$('auth').addEventListener('submit', (event) => { event.preventDefault(); authenticate($('token').value.trim()); });
$('arm').addEventListener('click', acquire);
$('stop').addEventListener('click', () => stopControl('Stopped. Take control to resume'));
$('throttle').addEventListener('input', () => { $('limit').textContent = `${$('throttle').value} / 255`; renderInput(); });
window.addEventListener('keydown', (event) => {
  if (event.code === 'Space' || event.code === 'Escape') {
    if (active || connecting || event.repeat || (event.code === 'Escape' && immersive)) event.preventDefault();
    if (active || connecting) stopControl('Stop requested from keyboard');
    if (event.code === 'Escape' && immersive) void leaveFullscreen();
    return;
  }
  const editing = event.target.matches?.('textarea,select,input:not([type=range])');
  if (event.code === 'KeyF' && !editing && !event.ctrlKey && !event.metaKey && !event.altKey) {
    event.preventDefault();
    if (!event.repeat) void toggleFullscreen();
    return;
  }
  const sliderArrow = event.target.type === 'range' && event.code.startsWith('Arrow');
  if (!active || !keyMap[event.code] || editing || sliderArrow || event.ctrlKey || event.metaKey || event.altKey) return;
  // Cancel native typing/scrolling on every driving keydown, including repeats
  // and paused input. Otherwise Gecko's find-as-you-type can steal focus.
  event.preventDefault();
  if (paused || event.repeat) return;
  keys.add(event.code); renderInput();
});
window.addEventListener('keyup', (event) => { keys.delete(event.code); renderInput(); });
window.addEventListener('blur', () => { if (active || connecting) stopControl('Window lost focus'); });
document.addEventListener('visibilitychange', () => { if (document.hidden && (active || connecting)) stopControl('Tab hidden'); });
window.addEventListener('pagehide', () => {
  if (active || connecting) {
    navigator.sendBeacon(stopUrl('Page closed'));
    stopControl('Page closed', false);
  }
});
document.querySelectorAll('[data-key]').forEach(button => {
  button.addEventListener('pointerdown', (event) => {
    if (!active || paused || (event.pointerType === 'mouse' && event.button !== 0)) return;
    event.preventDefault(); button.setPointerCapture(event.pointerId);
    pointers.set(event.pointerId, button.dataset.key); renderInput();
  });
  for (const name of ['pointerup','pointercancel','lostpointercapture']) {
    button.addEventListener(name, (event) => { pointers.delete(event.pointerId); renderInput(); });
  }
});
setInterval(() => {
  const now = performance.now();
  if (active && now - lastSample >= 250) pauseControl('Waiting for control connection');
  if (active && serverStatus?.camera_required && !hasFreshVideo()) pauseControl('Waiting for fresh video');
  if (lastRendered && now - lastRendered >= 700) {
    $('video-message').hidden = false;
    $('video-message').textContent = 'No fresh video';
  }
  renderStatus();
}, 100);
setInterval(pollStatus, 500);
if (token) authenticate(token);
