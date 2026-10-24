// Notifications on this device (Web Push): subscribe through the service worker, hand the
// subscription to the host over the signed-in session, keep the badge in step.
// iPhone and iPad allow this only once Glass is on the Home Screen.

const KINDS_KEY = 'glass.notify';

function storage(fn) {
  try { return fn(localStorage); } catch { return undefined; }
}

/** base64url → bytes (the host's VAPID key). */
function bytes(b64) {
  const s = atob(b64.replace(/-/g, '+').replace(/_/g, '/') + '==='.slice((b64.length + 3) % 4));
  return Uint8Array.from(s, (c) => c.charCodeAt(0));
}
const b64 = (buf) => btoa(String.fromCharCode(...new Uint8Array(buf))).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '');

function idbPut(key, value) {
  return new Promise((resolve) => {
    const req = indexedDB.open('glass', 1);
    req.onupgradeneeded = () => req.result.createObjectStore('kv');
    req.onsuccess = () => {
      const tx = req.result.transaction('kv', 'readwrite');
      if (value === undefined) tx.objectStore('kv').delete(key);
      else tx.objectStore('kv').put(value, key);
      tx.oncomplete = () => resolve();
      tx.onerror = () => resolve();
    };
    req.onerror = () => resolve();
  });
}

export function createPush({ control, log, toast, url, actions = true }) {
  const supported = 'serviceWorker' in navigator && 'PushManager' in window && 'Notification' in window;
  // iOS and iPadOS: Web Push only in a Home Screen web app.
  const ios = /iPhone|iPad/.test(navigator.userAgent) || (/Macintosh/.test(navigator.userAgent) && navigator.maxTouchPoints > 1);
  const standalone = matchMedia('(display-mode: standalone), (display-mode: fullscreen)').matches || navigator.standalone;
  let registration;
  let hostKey; // the host's VAPID public key, from welcome
  let owner = false;
  let subscription; // this browser's PushSubscription, if notifications are on
  let listeners = [];
  let kinds = { chat: true, asks: true, activity: false, ...JSON.parse(storage((s) => s.getItem(KINDS_KEY)) ?? '{}') };

  const ready = 'serviceWorker' in navigator
    ? navigator.serviceWorker.register('sw.js', { scope: './' }).then((r) => (registration = r)).catch((e) => log('warn', `service worker: ${e.message ?? e}`))
    : Promise.resolve();

  const changed = () => listeners.forEach((f) => f());

  function payload(sub) {
    const json = sub.toJSON();
    return { endpoint: json.endpoint, p256dh: json.keys.p256dh, auth: json.keys.auth, url, kinds };
  }

  async function current() {
    await ready;
    return registration?.pushManager.getSubscription().catch(() => null);
  }

  return {
    /** 'on', 'off', 'blocked', 'install' (add to Home Screen first), 'unsupported', 'unavailable' (host can't). */
    get state() {
      if (ios && !standalone && !supported) return 'install';
      if (!supported) return 'unsupported';
      if (!hostKey) return 'unavailable';
      if (Notification.permission === 'denied') return 'blocked';
      return subscription ? 'on' : 'off';
    },
    get kinds() { return kinds; },
    get owner() { return owner; },
    onChange(f) { listeners.push(f); },

    /** From welcome: re-sends an existing subscription (it may be new to this host). */
    async connected({ key, isOwner }) {
      hostKey = key;
      owner = isOwner;
      subscription = undefined;
      if (supported && key && Notification.permission === 'granted') {
        const sub = await current();
        // Made for another key (the host's was replaced): start over.
        const made = sub?.options?.applicationServerKey;
        if (sub && made && b64(made) !== key) await sub.unsubscribe().catch(() => {});
        else if (sub) {
          subscription = sub;
          control({ t: 'pushSubscribe', subscription: payload(sub) });
          control({ t: 'pushSeen', endpoint: sub.endpoint });
        }
      }
      navigator.clearAppBadge?.().catch(() => {});
      changed();
    },

    /** Must run inside a tap or click (the permission prompt). */
    async enable() {
      if (this.state === 'install') {
        toast({ text: 'To get notifications, add Glass to your Home Screen (Share › Add to Home Screen), then open it from there.', timeout: 9000 });
        return;
      }
      try {
        const permission = await Notification.requestPermission();
        if (permission !== 'granted') {
          toast({ text: permission === 'denied' ? 'Notifications are blocked for this site in the browser\'s settings.' : 'Notifications not turned on.', kind: 'error' });
          changed();
          return;
        }
        await ready;
        const sub = (await current()) ?? await registration.pushManager.subscribe({ userVisibleOnly: true, applicationServerKey: bytes(hostKey) });
        subscription = sub;
        control({ t: 'pushSubscribe', subscription: payload(sub) });
        // Keep the service worker and its subscription from being cleared to save space.
        navigator.storage?.persist?.().catch(() => {});
        log('info', 'notifications on');
      } catch (e) {
        toast({ text: `Couldn't turn on notifications: ${e.message ?? e}`, kind: 'error' });
        log('warn', `push subscribe failed: ${e.message ?? e}`);
      }
      changed();
    },

    async disable() {
      const sub = subscription ?? await current();
      if (sub) {
        control({ t: 'pushUnsubscribe', endpoint: sub.endpoint });
        await sub.unsubscribe().catch(() => {});
      }
      subscription = undefined;
      await idbPut('pushKey', undefined);
      log('info', 'notifications off');
      changed();
    },

    setKind(kind, on) {
      kinds = { ...kinds, [kind]: on };
      storage((s) => s.setItem(KINDS_KEY, JSON.stringify(kinds)));
      if (subscription) control({ t: 'pushSubscribe', subscription: payload(subscription) });
      changed();
    },

    /** The host's answer to a subscription. */
    async onMessage({ key }) {
      if (!key) {
        subscription = undefined;
        changed();
        return;
      }
      // Answers go to this page's host; a remote viewer's page isn't served by it.
      if (actions) await idbPut('pushKey', key);
    },

    /** Looking at Glass again: clear this device's badge. */
    seen() {
      navigator.clearAppBadge?.().catch(() => {});
      if (subscription) control({ t: 'pushSeen', endpoint: subscription.endpoint });
    },
  };
}
