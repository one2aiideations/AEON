// Runs src/player.js against a fake YouTube IFrame API and a fake network, and checks what it does.
// Usage: node player_js_harness.js path/to/player.js   (exit code 0 = every check passed)
'use strict';
const fs = require('fs');
const vm = require('vm');
const assert = require('assert');

const scriptPath = process.argv[2];
const posted = [];          // what the page reported to the engine
const polls = [];           // commands waiting to be handed to the page
let pollResolvers = [];     // a poll waiting for a command
const created = [];         // fake YouTube players
let intervalFn = null;
const timeouts = [];

global.window = global;
global.location = { pathname: '/t/TOKEN123/player', origin: 'http://127.0.0.1:4242' };
global.setInterval = (fn) => { intervalFn = fn; return 1; };
global.setTimeout = (fn, ms) => { timeouts.push(ms); return 1; };
global.fetch = (url, opts) => {
  if (url.endsWith('/event')) {
    assert.strictEqual(opts.method, 'POST');
    assert.strictEqual(opts.headers['Content-Type'], 'application/json');
    posted.push(JSON.parse(opts.body));
    return Promise.resolve({ json: () => ({}) });
  }
  assert.strictEqual(url, '/t/TOKEN123/cmd?wait=15', 'polls its own token path');
  if (polls.length) { const c = polls.shift(); return Promise.resolve({ json: () => c }); }
  return new Promise((resolve) => pollResolvers.push((c) => resolve({ json: () => c })));
};
global.YT = {
  PlayerState: { UNSTARTED: -1, ENDED: 0, PLAYING: 1, PAUSED: 2, BUFFERING: 3, CUED: 5 },
  Player: class {
    constructor(el, opts) { this.el = el; this.opts = opts; this.calls = []; created.push(this); }
    playVideo() { this.calls.push(['play']); }
    pauseVideo() { this.calls.push(['pause']); }
    stopVideo() { this.calls.push(['stop']); }
    seekTo(s, a) { this.calls.push(['seek', s, a]); }
    setVolume(v) { this.calls.push(['volume', v]); }
    setPlaybackRate(v) { this.calls.push(['rate', v]); }
    loadVideoById(id) { this.calls.push(['load', id]); }
    getDuration() { return 31.7; }
    getCurrentTime() { return 12.9; }
  }
};

const tick = () => new Promise((r) => setImmediate(r));
async function settle() { for (let i = 0; i < 5; i++) { await tick(); } }
async function send(cmd) {
  if (pollResolvers.length) { pollResolvers.shift()(cmd); } else { polls.push(cmd); }
  await settle();
}

(async () => {
  vm.runInThisContext(fs.readFileSync(scriptPath, 'utf8'), { filename: 'player.js' });
  await settle();

  // 1. A load that arrives before YouTube's API is ready waits, then creates the player.
  await send({ op: 'load', id: 'aaaaaaaaaaa' });
  assert.strictEqual(created.length, 0, 'no player before the API is ready');
  window.onYouTubeIframeAPIReady();
  assert.strictEqual(created.length, 1);
  const p = created[0];
  assert.strictEqual(p.opts.videoId, 'aaaaaaaaaaa');
  assert.strictEqual(p.opts.playerVars.autoplay, 1);
  assert.strictEqual(p.opts.playerVars.origin, 'http://127.0.0.1:4242', 'YouTube is told where the player runs');
  assert.strictEqual(p.el, 'p');
  p.opts.events.onReady({ target: p });
  assert.deepStrictEqual(p.calls.pop(), ['play']);

  // 2. Reports: loaded once with the length, playing, position, paused, ended.
  p.opts.events.onStateChange({ data: 1 });
  p.opts.events.onStateChange({ data: 1 });
  assert.deepStrictEqual(posted.splice(0), [{ event: 'loaded', duration: 31 }, { event: 'playing' }, { event: 'playing' }], 'loaded is reported once per video');
  intervalFn();
  assert.deepStrictEqual(posted.splice(0), [{ event: 'position', secs: 12 }]);
  p.opts.events.onStateChange({ data: 2 });
  intervalFn();
  assert.deepStrictEqual(posted.splice(0), [{ event: 'paused' }], 'no position reports while paused');
  p.opts.events.onStateChange({ data: 1 });
  posted.length = 0;
  p.opts.events.onStateChange({ data: 0 });
  assert.deepStrictEqual(posted.splice(0), [{ event: 'ended' }]);
  p.opts.events.onStateChange({ data: 3 });
  p.opts.events.onStateChange({ data: 5 });
  assert.deepStrictEqual(posted.splice(0), [], 'buffering and cued say nothing');

  // 3. The next load reuses the player and reports "loaded" again.
  await send({ op: 'load', id: 'bbbbbbbbbbb' });
  assert.deepStrictEqual(p.calls.pop(), ['load', 'bbbbbbbbbbb']);
  assert.strictEqual(created.length, 1, 'one player is reused');
  p.opts.events.onStateChange({ data: 1 });
  assert.deepStrictEqual(posted.splice(0), [{ event: 'loaded', duration: 31 }, { event: 'playing' }]);

  // 4. Only a well-formed video id ever reaches YouTube's player.
  for (const id of ['../../x', 'short', 'abcdefghijkl', 'abc"defghij', 'abc defghij', '', null, 5, ['aaaaaaaaaaa'], { toString() { return 'aaaaaaaaaaa'; } }]) {
    const before = p.calls.length;
    await send({ op: 'load', id });
    assert.strictEqual(p.calls.length, before, 'refused id: ' + JSON.stringify(id));
  }

  // 5. The other commands call the matching function, only with numbers where numbers belong.
  p.calls.length = 0;
  await send({ op: 'pause' }); await send({ op: 'play' }); await send({ op: 'stop' });
  await send({ op: 'seek', secs: 42 }); await send({ op: 'volume', value: 55 }); await send({ op: 'rate', value: 1.25 });
  assert.deepStrictEqual(p.calls, [['pause'], ['play'], ['stop'], ['seek', 42, true], ['volume', 55], ['rate', 1.25]]);
  p.calls.length = 0;
  await send({ op: 'seek', secs: '42' }); await send({ op: 'volume', value: '55' }); await send({ op: 'rate' });
  await send({ op: 'explode' }); await send({}); await send(null); await send({ op: 5 }); await send('load');
  assert.deepStrictEqual(p.calls, [], 'wrong types and unknown commands do nothing');

  // 6. A player error is reported with its code.
  p.opts.events.onError({ data: 153 });
  assert.deepStrictEqual(posted.splice(0), [{ event: 'error', code: 153 }]);

  // 7. The page keeps asking for commands after each one.
  assert.ok(pollResolvers.length === 1, 'exactly one poll is waiting');

  console.log('player.js: all checks passed');
})().catch((e) => { console.error('FAILED:', e && e.stack || e); process.exit(1); });
