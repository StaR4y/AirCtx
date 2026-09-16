# npm 平台子包打包说明

主包 `airctx` 通过 `optionalDependencies` 引用平台子包,安装时 npm 按 `os`/`cpu`
自动只装匹配当前平台的那一个。Linux 统一使用 musl 静态二进制(兼容一切发行版)。

## 平台子包模板

每个 `@airctx/cli-<platform>` 的 `package.json`:

```json
{
  "name": "@airctx/cli-darwin-arm64",
  "version": "0.1.0",
  "description": "AirCtx binary for darwin-arm64",
  "license": "MIT",
  "os": ["darwin"],
  "cpu": ["arm64"],
  "files": ["bin/"]
}
```

包内只含一个文件 `bin/airctx`(Windows 为 `bin/airctx.exe`)。

## CI 组装流程(发布脚本伪码)

```bash
# 1. 交叉编译产物(release 矩阵)
#    darwin-arm64 / darwin-x64 / linux-x64-musl / linux-arm64-musl / win32-x64

# 2. 组装平台包
for pkg in darwin-arm64 darwin-x64 linux-arm64 linux-x64 win32-x64; do
  dir=dist/npm/${pkg}
  mkdir -p ${dir}/bin
  cp target/<triple>/release/airctx* ${dir}/bin/
  # 填入上面的模板 package.json(os/cpu 相应调整)
done

# 3. 发布(版本号与主包一致)
npm publish dist/npm/darwin-arm64 --access public
# ... 其余平台
npm publish npm/ --access public
```

注意:
- 版本号三处保持一致:主包、平台包、Cargo workspace version
- 二进制在 CI 里应做 minisign 签名 + SHA256 校验和(roadmap)
