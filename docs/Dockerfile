# ── Build stage ─────────────────────────────────────────────────────────────
FROM node:20-alpine AS build
WORKDIR /app

# Install deps with a clean lockfile-driven install for reproducible builds.
COPY docs/package.json docs/package-lock.json* ./
RUN npm ci --no-audit --no-fund

# Bring in the rest of the Docusaurus project and build the static site.
COPY docs/ ./
RUN npm run build

# ── Runtime stage ───────────────────────────────────────────────────────────
FROM nginx:1.27-alpine

# Drop the default config and copy ours.
RUN rm /etc/nginx/conf.d/default.conf
COPY docs/nginx.conf /etc/nginx/conf.d/default.conf

# Static site assets.
COPY --from=build /app/build /usr/share/nginx/html

EXPOSE 80

HEALTHCHECK --interval=30s --timeout=3s --retries=3 \
  CMD wget -qO- http://127.0.0.1/healthz >/dev/null || exit 1
