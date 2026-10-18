// The minimal service worker that makes lastmile installable, so it can be
// a share target on Android. It caches nothing: every request goes to the
// network, as package pages must always be current.
self.addEventListener('install', () => self.skipWaiting());
self.addEventListener('activate', (event) => event.waitUntil(self.clients.claim()));
self.addEventListener('fetch', () => {});
