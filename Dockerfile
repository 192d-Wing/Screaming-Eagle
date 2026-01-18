# Build stage
FROM rust:1.75-alpine AS builder

RUN apk add --no-cache musl-dev openssl-dev openssl-libs-static pkgconfig

WORKDIR /app

# Copy manifests first for better caching
COPY Cargo.toml Cargo.lock ./

# Create dummy src to build dependencies
RUN mkdir src && echo "fn main() {}" > src/main.rs
RUN cargo build --release && rm -rf src

# Copy actual source code
COPY src ./src

# Build the actual binary
RUN touch src/main.rs && cargo build --release

# Runtime stage
FROM alpine:3.19

RUN apk add --no-cache ca-certificates

WORKDIR /app

# Copy binary from builder
COPY --from=builder /app/target/release/screaming-eagle /app/screaming-eagle

# Copy default config
COPY config /app/config

# Create non-root user
RUN addgroup -S cdn && adduser -S cdn -G cdn
RUN chown -R cdn:cdn /app
USER cdn

EXPOSE 8080

ENV CDN_CONFIG=/app/config/cdn.toml
ENV RUST_LOG=info

ENTRYPOINT ["/app/screaming-eagle"]
