const CACHE_NAME = "v4";
const ASSETS_TO_CACHE = ["/", "/favicon.ico"];

self.addEventListener("install", (event) => {
  event.waitUntil(
    caches.open(CACHE_NAME)
      .then((cache) => cache.addAll(ASSETS_TO_CACHE))
      .then(() => self.skipWaiting()),
  );
});

self.addEventListener("activate", (event) => {
  event.waitUntil(
    caches.keys()
      .then((cacheNames) =>
        Promise.all(
          cacheNames
            .filter((cacheName) => cacheName !== CACHE_NAME)
            .map((cacheName) => caches.delete(cacheName)),
        )
      )
      .then(() => self.clients.claim()),
  );
});

self.addEventListener("fetch", (event) => {
  const url = new URL(event.request.url);
  if (
    event.request.method !== "GET" ||
    url.origin !== self.location.origin ||
    url.pathname.startsWith("/api") ||
    url.pathname === "/healthz"
  ) {
    return;
  }

  if (url.pathname === "/") {
    event.respondWith(
      fetch(event.request)
        .then((response) => {
          if (response.ok) {
            const copy = response.clone();
            event.waitUntil(
              caches.open(CACHE_NAME)
                .then((cache) => cache.put("/", copy))
                .catch(() => {}),
            );
          }
          return response;
        })
        .catch(() =>
          caches.match("/").then((cached) => cached || new Response("Offline", { status: 503 }))
        ),
    );
    return;
  }

  if (event.request.mode === "navigate") {
    return;
  }

  event.respondWith(
    caches.match(event.request).then((response) => response || fetch(event.request)),
  );
});
