# Build Stage
FROM rust:1-alpine3.20 AS builder
WORKDIR /build
RUN apk update
RUN apk upgrade
ENV RUSTFLAGS="-C target-feature=-crt-static"
ENV CARGO_NET_GIT_FETCH_WITH_CLI=true
RUN apk add llvm cmake gcc ca-certificates libc-dev pkgconfig musl-dev git
COPY . .
RUN cargo build --release

FROM alpine:3.20
WORKDIR /run
RUN apk update
RUN apk upgrade
RUN apk add libgcc gcompat ca-certificates 
COPY --from=builder /build/target/release/bakta_web_backend .
COPY --from=builder /build/.env .
CMD [ "/run/bakta_web_backend" ]