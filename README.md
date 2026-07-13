🚧 Work in progress / 开发中

## CI 与发布

- 主分支和 Pull Request 执行前端构建及四平台 Rust 检查
- 推送与应用版本一致的 `v*` 标签后构建并发布安装包
- macOS Release 使用免费 Ad-hoc 签名，首次启动需要用户手动放行

配置方法见 [CI 发布说明](docs/ci-release.md)
