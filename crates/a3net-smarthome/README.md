# A3Net Smart Home Hub

智能家居设备控制中心 - 集成在 A3Net NAS 服务器中，支持多种智能家居协议和设备。

## 功能特性

### 协议支持

| 协议 | 描述 | 设备类型 |
|------|------|----------|
| **MIoT** | 小米智能家居云端/本地协议 | Xiaomi 设备 |
| **Matter** | 统一智能家居标准 ( CHIP ) | Matter 认证设备 |
| **MQTT** | 消息队列遥测传输 | 第三方系统集成 |
| **mDNS** | 局域网设备自动发现 | 本地设备 |

### 核心能力

- **设备管理**: 统一的设备注册、状态跟踪、在线/离线检测
- **场景控制**: 一键触发多个设备的预设状态组合
- **自动化引擎**: 基于事件和时间条件的自动化规则
- **REST API**: 完整的 HTTP API 用于外部集成
- **HomeKit 兼容**: 通过 Matter 协议与 Apple Home app 无缝对接

## 快速开始

### Rust 依赖

```toml
[dependencies]
a3net-smarthome = "0.1"
```

### 基本用法

```rust
use a3net_smarthome::{SmartHomeHub, HubConfig, miot::MiotAuth};
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 创建 Hub 配置
    let config = HubConfig::default();
    
    // 创建 Hub 并配置 MIoT 云端认证
    let auth = MiotAuth {
        user_id: "your_user_id".into(),
        service_token: "your_token".into(),
        device_id: "your_device_id".into(),
        ssecurity: "your_ssecurity".into(),
    };
    
    let hub = SmartHomeHub::new(config)
        .with_miot(auth)?;
    
    // 启动 Hub
    let hub = Arc::new(hub);
    hub.clone().start().await?;
    
    // 列出设备
    let devices = hub.list_devices().await;
    println!("发现 {} 个设备", devices.len());
    
    Ok(())
}
```

### 启动 REST API

```rust
use a3net_smarthome::api::{self, ApiConfig};

let api_config = ApiConfig {
    bind: "0.0.0.0:8781".parse().unwrap(),
    auth_token: Some("your-secret-token".into()),
};

let handle = api::serve(hub.clone(), api_config).await?;
println!("API 服务运行在 {}", handle.local_addr());
```

## REST API

### 设备管理

| 方法 | 路径 | 描述 |
|------|------|------|
| GET | `/api/devices` | 列出所有设备 |
| GET | `/api/devices/:id` | 获取单个设备 |
| POST | `/api/devices` | 注册设备 |
| DELETE | `/api/devices/:id` | 删除设备 |
| POST | `/api/devices/:id/properties/get` | 读取 MIoT 属性 |
| POST | `/api/devices/:id/properties/set` | 设置 MIoT 属性 |
| POST | `/api/devices/:id/action` | 调用设备动作 |

### Matter 设备

| 方法 | 路径 | 描述 |
|------|------|------|
| GET | `/api/matter/nodes` | 列出 Matter 节点 |
| POST | `/api/matter/commission` | 配对 Matter 设备 |

### 场景管理

| 方法 | 路径 | 描述 |
|------|------|------|
| GET | `/api/scenes` | 列出所有场景 |
| POST | `/api/scenes` | 创建场景 |
| GET | `/api/scenes/:id` | 获取场景详情 |
| DELETE | `/api/scenes/:id` | 删除场景 |
| POST | `/api/scenes/activate` | 激活场景 |

### 自动化规则

| 方法 | 路径 | 描述 |
|------|------|------|
| GET | `/api/automations` | 列出自动化规则 |
| POST | `/api/automations` | 创建自动化规则 |
| DELETE | `/api/automations/:id` | 删除自动化规则 |

### HomeKit 配件

| 方法 | 路径 | 描述 |
|------|------|------|
| GET | `/api/homekit/accessories` | 列出 HomeKit 配件 |

### 健康检查

| 方法 | 路径 | 描述 |
|------|------|------|
| GET | `/healthz` | 服务健康状态 |

## API 请求示例

### 读取设备属性

```bash
curl -X POST http://localhost:8781/api/devices/lamp-1/properties/get \
  -H "Authorization: Bearer your-token" \
  -H "Content-Type: application/json" \
  -d '{
    "properties": [{"siid": 2, "piid": 1}]
  }'
```

### 设置设备属性

```bash
curl -X POST http://localhost:8781/api/devices/lamp-1/properties/set \
  -H "Authorization: Bearer your-token" \
  -H "Content-Type: application/json" \
  -d '{
    "siid": 2,
    "piid": 1,
    "value": true
  }'
```

### 创建场景

```bash
curl -X POST http://localhost:8781/api/scenes \
  -H "Authorization: Bearer your-token" \
  -H "Content-Type: application/json" \
  -d '{
    "id": "movie-time",
    "name": "Movie Time",
    "room": "Living Room"
  }'
```

### 创建自动化规则

```bash
curl -X POST http://localhost:8781/api/automations \
  -H "Authorization: Bearer your-token" \
  -H "Content-Type: application/json" \
  -d '{
    "id": "night-light",
    "name": "Night Light",
    "enabled": true,
    "trigger": {
      "type": "property_changed",
      "device_id": "motion-1",
      "property": "3.1",
      "value": true
    },
    "conditions": [
      {
        "type": "time_in_range",
        "start": "20:00",
        "end": "06:00"
      }
    ],
    "actions": [
      {
        "type": "set_property",
        "device_id": "lamp-1",
        "siid": 2,
        "piid": 1,
        "value": true
      }
    ]
  }'
```

## MQTT 集成

MQTT 支持允许与 Home Assistant、OpenHAB 等第三方系统集成。

### 配置

```rust
use a3net_smarthome::mqtt::MqttConfig;

let mqtt_config = MqttConfig {
    host: "mqtt.home.local".into(),
    port: 1883,
    username: Some("user".into()),
    password: Some("pass".into()),
    topic_base: "home".into(),
    ..Default::default()
};
```

### MQTT 主题

| 主题 | 方向 | 描述 |
|------|------|------|
| `{base}/{device_id}/set` | 订阅 | 设备控制命令 |
| `{base}/{device_id}/{property}` | 发布 | 设备状态更新 |
| `{base}/{device_id}/availability` | 发布 | 设备在线状态 |
| `{base}/discovery` | 发布 | 设备发现消息 |

## 与 Apple HomeKit 集成

A3Net 通过 Matter 协议与 Apple Home app 无缝集成。Matter 是 HomeKit 的现代替代方案，支持原生 Apple HomeKit。

### 架构

```
┌─────────────────┐     ┌──────────────────┐     ┌─────────────────┐
│  A3Net Hub      │────▶│  Matter 协议     │────▶│  Apple Home App │
│  (MIoT 设备)   │     │  (统一标准)      │     │  (iOS/macOS)   │
└─────────────────┘     └──────────────────┘     └─────────────────┘
        │                       │
        │                       ▼
        │               ┌──────────────────┐
        └──────────────▶│  HomeKit 兼容性    │
                        │  (自动映射)       │
                        └──────────────────┘
```

### A3Net → Matter → HomeKit 映射表

| A3Net 能力 | Matter Cluster | HomeKit 等效 |
|------------|:--------------:|--------------|
| OnOff | 0x0006 | Switch/Light |
| Brightness | 0x0008 | Dimmable Light |
| Color | 0x0300 | Color Light |
| ColorTemperature | 0x0300 | Tunable White |
| Temperature | 0x0402 | Temperature Sensor |
| Humidity | 0x0405 | Humidity Sensor |
| Lock | 0x0101 | Door Lock |
| Motion | 0x0406 | Occupancy Sensor |

### 优点

- **零配置集成**: Matter 设备自动出现在 Apple Home app
- **跨平台兼容**: 同时支持 HomeKit、Google Home、Amazon Alexa
- **安全标准**: Matter 内置端到端加密
- **稳定可靠**: 使用成熟的生产级 Matter 实现

### 使用方法

1. 通过 A3Net CLI 或 REST API 配对 Matter 设备
2. 在 Apple Home app 中扫描 Matter 设置码
3. 直接从 Home app 或 Siri 控制设备

## 数据持久化

Hub 自动保存以下数据到 `data_dir`:

```
data/smarthome/
├── devices.json     # 设备注册表
├── automations.json # 自动化规则
└── scenes.json     # 场景定义
```

## 错误处理

```rust
use a3net_smarthome::{Result, SmartHomeError};

match hub.get_device("device-id").await {
    Some(device) => println!("找到: {}", device.name),
    None => println!("设备不存在"),
}
```

## 应用案例 (Use Cases / Examples)

下面是 A3Net 智能家居系统中常见的三个真实场景,对应
`examples/` 目录中的两个可运行示例:

1. **NAS 自带的极简控制中心** — 在没有 MIoT / Matter 设备的环境下,
   用 `SmartHomeHub::new(HubConfig::default()).start()` 启动 Hub,
   监听 `/api/healthz` 健康检查, 然后通过 `POST /api/devices` 手动
   注册本地虚拟设备, 让外部脚本通过 REST API 读取设备状态。完整
   流程见 `examples/smarthome_basic.rs`。

2. **联动小米设备 + HomeKit** — 用 `with_miot(MiotAuth { ... })`
   配置 MIoT 云端凭证, 用 `with_matter(...)` 把 Matter 控制器挂上,
   Hub 启动后会自动把 MIoT 设备属性映射到 Matter 集群, Apple Home
   app 扫码即可纳管同一台 NAS。

3. **“晚安”场景化 + 自动化** — 用 `SceneManager` 注册名为
   `night-mode` 的场景, 包含"关灯、关窗帘、降低空调温度"三条
   `SetProperty` 动作; 用 `Automation` 注册一个 `Schedule { cron: "23:30" }`
   触发器, 23:30 自动激活场景。Hub 内部的 `automation task` 会按
   分钟扫描并执行。运行时刷新场景可见
   `examples/smarthome_app.rs`。

## 许可

MIT OR Apache-2.0
