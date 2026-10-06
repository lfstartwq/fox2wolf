# fox2wolf

> Firefox → LibreWolf 历史记录迁移工具

将 Firefox 的 `places.sqlite` 历史记录（访问记录、URL、时间戳、frecency 等）迁移到 LibreWolf，支持**合并去重**、**大数据量单事务处理**、**进度显示**。

**语言**: [English](README.md) | [简体中文](README.zh-CN.md)

## 特性

- **合并去重**：同 URL 自动合并 `visit_count`，保留最早首次访问和最晚访问时间
- **大数据量优化**：>50 万条记录在单个事务内完成、关闭 `synchronous`、WAL 模式、进度条
- **自动 Profile 检测**：读取 `profiles.ini` 定位默认 Profile，支持手动指定
- **安全第一**：迁移前强制确认、源库只读打开、目标库事务保护、干运行模式
- **UTC 时间戳保持**：Firefox PRTime (微秒) 原样迁移，不做时区转换
- **跨平台**：Windows / Linux / macOS 均可用

## 依赖

| Crate | 版本 | 用途 |
|-------|------|------|
| `rusqlite` | 0.31 | SQLite 数据库访问（内置 SQLite） |
| `directories-next` | 2.0 | 跨平台标准目录路径（AppData 等） |
| `clap` | 4.5 | CLI 参数解析 |
| `uuid` | 1.8 | UUID v4 生成新 GUID |
| `chrono` | 0.4 | 日期时间处理（UTC 微秒） |
| `crc32fast` | 1.3 | URL hash 计算（兼容 Firefox 算法） |
| `indicatif` | 0.17 | 进度条 |
| `tracing` + `tracing-subscriber` | 0.1 / 0.3 | 结构化日志 |
| `toml` | 0.8 | profiles.ini 解析 |
| `walkdir` | 2.5 | 目录扫描（备选 Profile 发现） |
| `serde` + `serde_json` | 1.0 | 统计/元数据序列化 |
| `thiserror` | 1.0 | 错误处理 |

## 安装

### 从源码构建

```bash
git clone https://github.com/lfstartwq/fox2wolf
cd fox2wolf
cargo build --release
# 二进制文件在 target/release/fox2wolf(.exe)
```

## 使用方法

### 基本用法（自动检测默认 Profile）

```bash
fox2wolf
```

### 指定 Profile

```bash
# 指定 Firefox 和 LibreWolf 的 profile 名称
fox2wolf --firefox-profile default-release --librewolf-profile default-default

# 或指定完整路径
fox2wolf --firefox-profile "C:\Users\Name\AppData\Roaming\Mozilla\Firefox\Profiles\xyz.default-release" \
         --librewolf-profile "C:\Users\Name\AppData\Roaming\librewolf\Profiles\abc.default-default"
```

### 干运行（预览）

```bash
fox2wolf --dry-run
```

### 跳过确认（自动化脚本）

```bash
fox2wolf --yes
```

### 列出所有可用 Profile

```bash
fox2wolf --list-profiles
```

### 完整参数

```
Usage: fox2wolf [OPTIONS]

Options:
  -f, --firefox-profile <NAME|PATH>      Firefox profile 名称或路径
  -l, --librewolf-profile <NAME|PATH>    LibreWolf profile 名称或路径
      --dry-run                          仅模拟，不写入目标库
  -y, --yes                              跳过确认提示
      --list-profiles                    列出所有 profiles 并退出
      --log-level <LEVEL>                日志级别 [trace, debug, info, warn, error] (默认: info)
      --no-progress                      禁用进度条
  -h, --help                             显示帮助
  -V, --version                          显示版本
```

## 迁移前准备

**重要**：

1. **完全关闭 Firefox 和 LibreWolf**（检查任务管理器，确保无残留进程）
2. **手动备份 LibreWolf 的 `places.sqlite`**：
   - Windows: `%APPDATA%\librewolf\Profiles\<profile>\places.sqlite`
   - Linux: `~/.librewolf/<profile>/places.sqlite`
   - macOS: `~/Library/Application Support/librewolf/Profiles/<profile>/places.sqlite`
3. 备份建议重命名为 `places.sqlite.backup.$(date +%Y%m%d_%H%M%S)`

## 迁移内容

| 表 | 说明 | 处理方式 |
|---|---|---|
| `moz_origins` | 域名来源 | 去重合并 (host+prefix 唯一) |
| `moz_places` | URL 记录 | 合并去重 (url_hash+url)，生成新 GUID，重算 frecency |
| `moz_historyvisits` | 访问历史 | 去重 (place_id+visit_date+visit_type)，重写 place_id 映射 |

**不迁移**：书签、搜索关键字、输入历史、标注、页面元数据等（如需请手动导出/导入 JSON）。

## 实现细节

### ID 映射策略

- `moz_origins.id`：按 `(host, prefix)` 去重，建立 `old_id → new_id` 映射
- `moz_places.id`：按 `(url_hash, url)` 去重，建立映射，生成新 UUID v4 作为 GUID
- `moz_historyvisits.id`：自增重新分配，`place_id` 按映射表重写，`from_visit` 按访问顺序重链

### 合并规则

同 URL 存在时：
- `visit_count` 累加
- `hidden` 逻辑或
- `typed` 取最大
- `foreign_count` 累加
- `last_visit_date` 取最大（最晚访问）
- `frecency` 迁移完成后重算（见下方说明）

### UTF-8 处理

Firefox 的 `places.sqlite` 在 TEXT 列（如 `description`）中可能包含非法 UTF-8 序列。工具使用 `row.get_ref()` 配合 `String::from_utf8_lossy` 进行容错转换。

## 开发

**文档**: [docs/architecture.md](docs/architecture.md) | [docs/development.md](docs/development.md)

### 代码结构

```
fox2wolf/
├── src/
│   ├── main.rs          # CLI 入口
│   ├── lib.rs           # 公共 API 导出
│   ├── error.rs         # 错误类型
│   ├── models.rs        # 数据模型 (Origin, Place, Visit 等)
│   ├── profile.rs       # Profile 发现与验证
│   ├── db.rs            # SQLite 连接、PRAGMA、事务
│   ├── dedup.rs         # 合并/去重算法、ID 重映射
│   └── migrate.rs       # 迁移编排
├── tests/
│   └── integration_test.rs  # 端到端测试
├── docs/
│   ├── architecture.md              # 架构说明
│   ├── commit-message-guidelines.md # 提交信息规范
│   └── development.md               # 开发指南
├── Cargo.toml
├── Cargo.lock
├── README.md
└── README.zh-CN.md
```

## 许可证

GPL-3.0-only