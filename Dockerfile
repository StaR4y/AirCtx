# 多阶段构建:rust:alpine 原生 musl 编译 → scratch 最终镜像。
# 不要加 `# syntax=docker/dockerfile:1`:那会先去 Docker Hub 拉 frontend,
# Colima DNS 一坏(常见 [::1]:53 connection refused)构建直接失败。
# RUN --mount 由本机 BuildKit 内置 frontend 支持,无需额外镜像。
# 最终镜像只含一个静态二进制,无 shell/无 libc/无 CA,体积 ~10MB,
# 内存占用与裸跑完全一致。

FROM rust:1-alpine AS builder
RUN apk add --no-cache musl-dev
WORKDIR /build
COPY . .
# 缓存挂载:依赖与编译产物跨构建复用,源码变更只增量编译 workspace
RUN --mount=type=cache,target=/build/target \
    --mount=type=cache,target=/root/.cargo/registry \
    cargo build --release -p airctx \
    && cp target/release/airctx /airctx

FROM scratch
COPY --from=builder /airctx /airctx
VOLUME ["/data"]
EXPOSE 8787
# scratch 无 curl/wget; ping 从 /data/server.toml 读安全入口
HEALTHCHECK --interval=30s --timeout=5s --start-period=8s --retries=3 \
    CMD ["/airctx", "ping", "--data-dir", "/data"]
ENTRYPOINT ["/airctx"]
CMD ["serve", "--addr", "0.0.0.0:8787", "--data-dir", "/data"]
