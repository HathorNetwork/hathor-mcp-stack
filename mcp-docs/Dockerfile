FROM nginx:alpine

# curl makes healthchecks robust regardless of whether busybox wget is
# installed; it's tiny and worth it for deterministic routing.
RUN apk add --no-cache curl

COPY index.html /usr/share/nginx/html/index.html
COPY nginx.conf /etc/nginx/conf.d/default.conf

EXPOSE 80

HEALTHCHECK --interval=15s --timeout=5s --retries=3 --start-period=5s \
  CMD curl -fsS http://localhost/health || exit 1
