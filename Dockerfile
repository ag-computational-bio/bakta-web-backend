# Build Stage
FROM rust:1-alpine3.20 AS builder
WORKDIR /build
RUN apk update
RUN apk upgrade
ENV RUSTFLAGS="-C target-feature=-crt-static"
ENV CARGO_NET_GIT_FETCH_WITH_CLI=true
RUN apk add llvm cmake gcc ca-certificates libc-dev pkgconfig musl-dev git openssl-dev curl
COPY . .
RUN cargo build --release

FROM alpine:3.20
WORKDIR /run
RUN apk update
RUN apk upgrade
RUN apk add libgcc gcompat ca-certificates openssl-dev
COPY --from=builder /build/target/release/bakta-web-backend .
COPY --from=builder /build/.env .
CMD [ "/run/bakta-web-backend" ]