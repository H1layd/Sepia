/* sepia anti-fingerprint shim.
 *
 * Injected at document_start into the MAIN world of every frame. Everything it
 * touches is derived from the persona baked in below, so the same session sees
 * a stable machine and a new session sees a different one.
 *
 * The TZ and Accept-Language are handled natively by launching Chrome with TZ=
 * and --accept-lang, so this script never lies about the clock or languages:
 * it only fills in surfaces the browser cannot be told about.
 */
(function () {
  "use strict";

  var CFG = /*__SEPIA_CFG__*/ null;
  if (!CFG) return;
  // The re-entry guard must not be a stable global a page can probe: its name is
  // derived from the session seed and the property is non-enumerable.
  var GUARD = CFG.guard;
  if (window[GUARD]) return;
  Object.defineProperty(window, GUARD, { value: true, enumerable: false, configurable: false });

  var patched = new WeakSet();
  var realToString = Function.prototype.toString;
  Function.prototype.toString = function () {
    if (patched.has(this)) return "function () { [native code] }";
    return realToString.call(this);
  };
  try {
    Object.defineProperty(patched, "constructor", { value: WeakSet });
  } catch (e) {}

  function hide(fn) {
    patched.add(fn);
    return fn;
  }

  function prng(seed) {
    var s = seed >>> 0;
    return function () {
      s |= 0; s = (s + 0x6D2B79F5) | 0;
      var t = Math.imul(s ^ (s >>> 15), 1 | s);
      t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
      return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
    };
  }

  function defineGetter(obj, prop, getter) {
    try {
      var desc = Object.getOwnPropertyDescriptor(obj, prop);
      if (desc && !desc.configurable) return;
      Object.defineProperty(obj, prop, {
        configurable: true,
        enumerable: desc ? desc.enumerable : true,
        get: hide(getter)
      });
    } catch (e) {}
  }

  // ---------------------------------------------------------------- canvas
  var canvasRandom = prng(CFG.canvasSeed);
  function noisyImageData(data, strength) {
    var rnd = prng((CFG.canvasSeed ^ (data.length & 0xFFFF)) >>> 0);
    for (var i = 0; i < data.length; i += 4) {
      if (rnd() < 0.22) {
        data[i] = (data[i] + ((rnd() * strength) | 0)) & 255;
        data[i + 1] = (data[i + 1] + ((rnd() * strength) | 0)) & 255;
        data[i + 2] = (data[i + 2] + ((rnd() * strength) | 0)) & 255;
      }
    }
    return data;
  }

  if (window.CanvasRenderingContext2D) {
    var origGetImageData = CanvasRenderingContext2D.prototype.getImageData;
    CanvasRenderingContext2D.prototype.getImageData = hide(function getImageData() {
      var result = origGetImageData.apply(this, arguments);
      try { noisyImageData(result.data, 2); } catch (e) {}
      return result;
    });
  }

  if (window.HTMLCanvasElement) {
    function wrapExport(name) {
      var orig = HTMLCanvasElement.prototype[name];
      if (!orig) return;
      HTMLCanvasElement.prototype[name] = hide(function () {
        try {
          var ctx = this.getContext("2d");
          if (ctx) {
            var data = ctx.getImageData(0, 0, Math.max(1, this.width), Math.max(1, this.height));
            noisyImageData(data.data, 2);
            ctx.putImageData(data, 0, 0);
          }
        } catch (e) {}
        return orig.apply(this, arguments);
      });
    }
    ["toDataURL", "toBlob"].forEach(wrapExport);
  }

  // ------------------------------------------------------------------ webgl
  if (window.WebGLRenderingContext) {
    var origGetParameter = WebGLRenderingContext.prototype.getParameter;
    WebGLRenderingContext.prototype.getParameter = hide(function getParameter(p) {
      if (p === 37445) return CFG.webglVendor;
      if (p === 37446) return CFG.webglRenderer;
      return origGetParameter.apply(this, arguments);
    });
  }
  if (window.WebGL2RenderingContext) {
    var origGetParameter2 = WebGL2RenderingContext.prototype.getParameter;
    WebGL2RenderingContext.prototype.getParameter = hide(function getParameter(p) {
      if (p === 37445) return CFG.webglVendor;
      if (p === 37446) return CFG.webglRenderer;
      return origGetParameter2.apply(this, arguments);
    });
  }

  // ------------------------------------------------------------------ audio
  if (window.AudioBuffer) {
    var origChannelData = AudioBuffer.prototype.getChannelData;
    AudioBuffer.prototype.getChannelData = hide(function getChannelData() {
      var data = origChannelData.apply(this, arguments);
      try {
        var rnd = prng((CFG.audioSeed ^ data.length) >>> 0);
        for (var i = 0; i < data.length; i += 100) {
          data[i] = data[i] + (rnd() - 0.5) * 1e-7;
        }
      } catch (e) {}
      return data;
    });
  }
  if (window.AnalyserNode) {
    var origFloat = AnalyserNode.prototype.getFloatFrequencyData;
    AnalyserNode.prototype.getFloatFrequencyData = hide(function (array) {
      origFloat.apply(this, arguments);
      try {
        var rnd = prng(CFG.audioSeed >>> 0);
        for (var i = 0; i < array.length; i += 50) array[i] += (rnd() - 0.5) * 1e-4;
      } catch (e) {}
      return undefined;
    });
  }

  // ----------------------------------------------------------------- screen
  if (window.Screen) {
    defineGetter(Screen.prototype, "width", function () { return CFG.screen.width; });
    defineGetter(Screen.prototype, "height", function () { return CFG.screen.height; });
    defineGetter(Screen.prototype, "availWidth", function () { return CFG.screen.width; });
    defineGetter(Screen.prototype, "availHeight", function () {
      return CFG.screen.height - CFG.screen.taskbar;
    });
    defineGetter(Screen.prototype, "colorDepth", function () { return CFG.screen.colorDepth; });
    defineGetter(Screen.prototype, "pixelDepth", function () { return CFG.screen.colorDepth; });
  }
  defineGetter(window, "devicePixelRatio", function () { return CFG.screen.dpr; });

  // ------------------------------------------------------------- navigator
  defineGetter(Navigator.prototype, "hardwareConcurrency", function () {
    return CFG.hardwareConcurrency;
  });
  defineGetter(Navigator.prototype, "deviceMemory", function () { return CFG.deviceMemory; });
  defineGetter(Navigator.prototype, "maxTouchPoints", function () { return CFG.maxTouchPoints; });
  defineGetter(Navigator.prototype, "webdriver", function () { return false; });
  defineGetter(Navigator.prototype, "platform", function () { return CFG.platform; });

  if (navigator.connection) {
    defineGetter(Navigator.prototype, "connection", function () {
      return {
        rtt: 50 + (CFG.canvasSeed % 90),
        downlink: 9.5 + (CFG.audioSeed % 40) / 10,
        effectiveType: "4g",
        saveData: false,
        addEventListener: function () {},
        removeEventListener: function () {},
        onchange: null
      };
    });
  }

  // ----------------------------------------------------------------- webrtc
  if (window.RTCPeerConnection || window.webkitRTCPeerConnection) {
    var NativePC = window.RTCPeerConnection || window.webkitRTCPeerConnection;
    if (CFG.strictWebrtc) {
      var blocked = hide(function RTCPeerConnection() {
        throw new DOMException("WebRTC is disabled by sepia", "NotAllowedError");
      });
      window.RTCPeerConnection = blocked;
      window.webkitRTCPeerConnection = blocked;
    } else {
      var stripHostCandidates = function (sdp) {
        if (typeof sdp !== "string") return sdp;
        return sdp.split("\r\n").filter(function (line) {
          if (line.indexOf("a=candidate:") !== 0) return true;
          var parts = line.split(" ");
          return parts.length < 8 || parts[7] !== "host";
        }).join("\r\n");
      };

      var Proxied = hide(function RTCPeerConnection(config) {
        var cfg = config || {};
        cfg.iceServers = cfg.iceServers || [];
        return new NativePC(cfg);
      });
      Proxied.prototype = NativePC.prototype;
      window.RTCPeerConnection = Proxied;

      var origSetLocalDescription = NativePC.prototype.setLocalDescription;
      NativePC.prototype.setLocalDescription = hide(function setLocalDescription(desc) {
        if (desc && typeof desc === "object" && desc.sdp) {
          desc = { type: desc.type, sdp: stripHostCandidates(desc.sdp) };
        }
        return origSetLocalDescription.call(this, desc);
      });

      var origCreateOffer = NativePC.prototype.createOffer;
      NativePC.prototype.createOffer = hide(function createOffer() {
        var self = this;
        return origCreateOffer.apply(this, arguments).then(function (offer) {
          offer.sdp = stripHostCandidates(offer.sdp);
          return offer;
        });
      });
    }
  }

  // -------------------------------------------------- consistent UA header
  // Kept native on purpose: HTTP Client Hints, TLS and HTTP/2 fingerprints all
  // come from the same Chrome build, so the JS view must not contradict them.
  try {
    Object.defineProperty(navigator, "languages", {
      configurable: true,
      get: hide(function () { return CFG.languages.slice(); })
    });
    Object.defineProperty(navigator, "language", {
      configurable: true,
      get: hide(function () { return CFG.languages[0]; })
    });
  } catch (e) {}
})();
