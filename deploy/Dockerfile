# Builds the web app and the Rust server into one small image.
FROM node:22-slim AS web
WORKDIR /web
COPY web/package.json ./
RUN npm install --no-audit --no-fund
COPY web/ ./
RUN npm run build

FROM rust:1-slim AS server
WORKDIR /server
COPY server/ ./
RUN cargo build --release

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 sourcer
COPY --from=server /server/target/release/sourcer-server /usr/local/bin/sourcer-server
COPY --from=web /web/dist /srv/web
USER sourcer
EXPOSE 8080
CMD ["sourcer-server"]
