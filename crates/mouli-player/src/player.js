// The page that runs YouTube's own player (the IFrame API) for MOULI-PLAYER.
// It does exactly two things: do what the engine says (load, play, pause, seek, volume, rate, stop)
// and tell the engine what the player reports (loaded, playing, paused, ended, position, error).
// It never touches video data: YouTube's player does all of that, with its ads and rules.
(function () {
  'use strict';
  var base = location.pathname.replace(/\/player$/, '');
  var player = null;
  var apiReady = false;
  var wanted = null;
  var loadedSent = false;
  var state = -2;

  function post(o) {
    return fetch(base + '/event', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(o)
    }).catch(function () {});
  }

  function create(id) {
    player = new YT.Player('p', {
      videoId: id,
      width: '100%',
      height: '100%',
      playerVars: { autoplay: 1, playsinline: 1, rel: 0, origin: location.origin },
      events: {
        onReady: function (e) { e.target.playVideo(); },
        onStateChange: onState,
        onError: function (e) { post({ event: 'error', code: e.data }); }
      }
    });
  }

  function load(id) {
    loadedSent = false;
    if (player && player.loadVideoById) { player.loadVideoById(id); }
    else if (apiReady) { create(id); }
    else { wanted = id; }
  }

  window.onYouTubeIframeAPIReady = function () {
    apiReady = true;
    if (wanted) { var id = wanted; wanted = null; create(id); }
  };

  function onState(e) {
    state = e.data;
    var S = YT.PlayerState;
    if (state === S.PLAYING) {
      if (!loadedSent) {
        loadedSent = true;
        post({ event: 'loaded', duration: Math.floor(player.getDuration() || 0) });
      }
      post({ event: 'playing' });
    } else if (state === S.PAUSED) {
      post({ event: 'paused' });
    } else if (state === S.ENDED) {
      post({ event: 'ended' });
    }
  }

  setInterval(function () {
    if (player && state === 1 && player.getCurrentTime) {
      post({ event: 'position', secs: Math.floor(player.getCurrentTime()) });
    }
  }, 1000);

  function handle(c) {
    if (!c || typeof c.op !== 'string') { return; }
    switch (c.op) {
      case 'load':
        // Only a well-formed video id is ever passed on to YouTube's player.
        if (typeof c.id === 'string' && /^[A-Za-z0-9_-]{11}$/.test(c.id)) { load(c.id); }
        break;
      case 'pause': if (player) { player.pauseVideo(); } break;
      case 'play': if (player) { player.playVideo(); } break;
      case 'seek': if (player && typeof c.secs === 'number') { player.seekTo(c.secs, true); } break;
      case 'volume': if (player && typeof c.value === 'number') { player.setVolume(c.value); } break;
      case 'rate': if (player && typeof c.value === 'number') { player.setPlaybackRate(c.value); } break;
      case 'stop': if (player) { player.stopVideo(); } break;
      default: break;
    }
  }

  function poll() {
    fetch(base + '/cmd?wait=15')
      .then(function (r) { return r.json(); })
      .then(function (c) { handle(c); poll(); })
      .catch(function () { setTimeout(poll, 1000); });
  }
  poll();
})();
