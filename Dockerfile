FROM postgres:17-bookworm AS database

FROM rust:1-bookworm AS build
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --locked --release

FROM build AS test
RUN cargo test --locked --no-run
CMD ["cargo", "test", "--locked", "--offline", "--", "--include-ignored"]

FROM debian:bookworm-slim AS server
RUN groupadd --gid 10001 gemini && useradd --uid 10001 --gid gemini --no-create-home gemini
COPY --from=build /app/target/release/poc_server /usr/local/bin/poc_server
USER gemini
EXPOSE 26000/tcp
HEALTHCHECK --interval=30s --timeout=5s --start-period=30s --retries=3 CMD ["/usr/local/bin/poc_server", "--health-check"]
ENTRYPOINT ["/usr/local/bin/poc_server"]
CMD ["--host", "0.0.0.0", "--port", "26000"]
