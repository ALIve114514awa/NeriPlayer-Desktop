# NeriPlayer Windows (dotnet-windows)

> ⚠️ **本目录是独立的 .NET 技术栈实现方案，与仓库根目录的 Tauri (Rust + Vue) 官方实现互不干扰。**

## 这是什么

这是将 NeriPlayer 移植到 Windows 桌面端的 **.NET 8 + Avalonia UI** 实现方案，与官方 `NeriPlayer-Desktop`（Tauri 2 + Rust + Vue 3）为**平行独立的两套技术栈**。

- 本目录代码不参与仓库根目录的 `pnpm` / `cargo` 构建体系
- 保留 3 个历史提交（脚手架 → 核心数据模型 → 对齐修复）
- 作为技术方案参考与对比，供社区评估不同实现路线

## 技术栈

| 组件 | 选型 |
|------|------|
| 运行时 | .NET 8 LTS |
| UI | Avalonia UI 11.x |
| 播放引擎 | LibVLCSharp 8.x（VLC 3.0.x） |
| 数据库 | EF Core 8 + SQLite |
| 音效 | NAudio (WASAPI) / Biquad 滤波器 |
| 系统集成 | SMTC / 托盘 / Toast |

## 项目结构

```
src/
├── NeriPlayer.App/         主应用入口（Avalonia Desktop）
├── NeriPlayer.Core/        核心业务层（播放/歌词/下载/策略）
├── NeriPlayer.Data/        数据层（EF Core / 同步）
├── NeriPlayer.UI/          UI 层（Avalonia 视图）
└── NeriPlayer.Background/  后台服务（SMTC / 托盘）
tests/                      单元测试（xunit）
```

## 构建与运行

```powershell
dotnet build NeriPlayer.Windows.sln
dotnet test tests/NeriPlayer.Core.Tests
dotnet run --project src/NeriPlayer.App
```

> 详细实施方案见 `Analysis.md`（源码分析 24 章）与 `Process.md`（移植方案 19 章）。
