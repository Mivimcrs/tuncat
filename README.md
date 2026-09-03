# TunCat 🐱

**TunCat** 是一个 Windows 托盘常驻小工具：自动修复「安装 OPPO 互联后 Clash/Mihomo TUN 模式无法联网」的问题。

## 原理

Mihomo 的 TUN 模式使用 Wintun 虚拟网卡接管系统流量。OPPO 互联的网络组件会干扰 Windows 对新建虚拟网卡的初始化（NLA 识别、路由、DNS），导致网卡虽然 Up 但流量进得去出不来。

TunCat 的修复手段是 **ICS 脉冲**：短暂开启一次 Windows「Internet 连接共享」（TUN 网卡为 private 端、物理网卡为 public 端），强制 Windows 走完整网卡初始化流程，2 秒后关闭共享。网卡被"登记在册"后，Mihomo 重新接管即恢复正常。

该方案源自社区验证的 PowerShell 脚本，TunCat 将其工程化：

- 纯 Rust 原生 COM 调用（`IDispatch` 后期绑定 `HNetCfg.HNetShare`），不依赖 PowerShell 子进程
- 修复前**快照**已有共享，修复后**恢复**（原脚本会清掉你的长期共享不还）
- 连续修复失败自动熔断，绝不无限循环
- 修复期间网络会闪断约 2 秒，属预期行为

## 功能

- 🖥️ 三页签界面：状态 / 日志 / 设置，深色 & 浅色主题（可跟随系统）
- 🐱 托盘常驻，图标颜色实时反映状态：绿=正常、黄=修复中、红=异常、灰=已暂停
- ⏰ 开机静默自启（计划任务最高权限运行，无 UAC 弹窗、无窗口）
- 🔁 周期检测（可配间隔/探测地址/失败阈值），TUN 异常时自动修复
- 🖱️ 手动「立即检测」「立即修复」；关闭窗口最小化到托盘
- 📋 滚动文件日志（%APPDATA%\TunCat\logs\，保留 7 天）

## 使用

1. 从 [Releases](../../releases) 下载 `tuncat-x.y.z.zip`，解压运行 `tuncat.exe`（需管理员权限）。
2. 首次启动后进入「设置」开启「开机自动启动」。
3. 日常无需任何操作：TUN 挂了它自己修，TUN 正常时它只安静待在托盘里。

### 配置文件

`%APPDATA%\TunCat\config.json`，全部字段均可在设置页修改，说明见注释与文档。

### 命令行参数

| 参数 | 作用 |
|---|---|
| `--silent` | 启动时不显示主窗口，仅驻托盘（开机自启用） |

## 构建

需要 Windows 10/11 + Rust（MSVC 工具链）：

```bash
cargo build --release
```

产物：`target/release/tuncat.exe`。CI 会为每个 tag 自动构建发布包。

## 已知限制

- 仅支持 Windows 10/11 x64（ICS 是 Windows 独有机制）
- 探测地址默认 `gstatic generate_204`：TUN 正常时经代理可达，TUN 故障时直连也不可达，判定准确；若改成国内直连可达的地址会造成误判健康
- OPPO 互联未来版本若改变干扰机制，脉冲方案可能失效——程序会进入「已停止自动修复」状态并在托盘提示，而不是盲目重试
- 请确保使用时 Clash/Mihomo 的 TUN 模式处于开启状态，否则程序会显示「未发现 TUN 网卡」并不做任何事

## 致谢

- ICS 脉冲修复思路来自社区 Mihomo/Clash 用户的 PowerShell 脚本实践
- [windows-rs](https://github.com/microsoft/windows-rs) / [egui](https://github.com/emilk/egui) / [tray-icon](https://github.com/justchoko/tray-icon)

## License

MIT
