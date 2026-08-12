# 真实网络验收矩阵

## 目标与边界

本套件验证 ADNet 的真实 iroh 数据面，不使用 loopback、中继模拟器或单进程替身。拓扑固定为：

- 一台自托管 `iroh-relay`，必须使用非 `iroh.link` 域名；
- 公网节点 A：监听可入站 UDP 端口，提供 `frame/blobs/gossip/docs`；
- 公网控制节点 B：SSH 控制面可达，ADNet 工作负载运行在受限 NAT 网络命名空间中；
- GitHub Actions 控制器：仅构建、SSH 部署、触发场景和收集证据。

受限 NAT 与断网必须只作用于 B 的工作负载网络命名空间，不能切断 SSH 控制面。`network_acceptance probe --path relay` 还会调用 iroh 的 `clear_ip_transports()`，因此 relay 场景无法静默走直连。

## 验收矩阵

| ID | 场景 | 故障/路径约束 | 通过判定 | 主要证据 |
|---|---|---|---|---|
| RN-01 | 自托管 relay 健康 | relay 域名不得属于公共 n0 基础设施 | 健康 URL 返回 `< 400` | relay URL、HTTP 状态 |
| RN-02 | 两公网节点部署 | SSH host key 固定，二进制 SHA-256 复核 | 两端部署同一构建产物 | 构建与部署 case |
| RN-02A | 受限 NAT 拓扑证明 | B 工作负载在独立 namespace，只有出站默认路由 | topology check 命令成功并保存接口/路由 | 命令及 stdout |
| RN-03 | 直连 | A 的公网 UDP 地址是唯一拨号提示 | QUIC 选中 IP path 且 frame echo 一致 | `selected_path=direct` |
| RN-04 | relay 回退 | B 清除全部 IP transport，只保留自托管 relay | QUIC 选中 relay path 且 frame echo 一致 | `selected_path=relay` |
| RN-05 | 重连 | 每轮主动关闭连接，共 3 轮 | 三次新连接均成功并保持 relay path | attempts、selected paths |
| RN-06 | blobs | Bao/iroh-blobs 经 relay 下载固定载荷 | hash 可取回且内容逐字节一致 | hash、bytes |
| RN-07 | gossip | B 以 A 为 bootstrap，发送 nonce | A 返回 `ack:<nonce>` | ack、耗时 |
| RN-08 | docs | B 导入 A 的写票据并进行初始同步 | 读到 A 的消息，B 写入后本地可读 | 初始/写后消息数 |
| RN-09 | 服务重启恢复 | 停止 A，保持持久身份与磁盘状态后重启 | endpoint ID 不变；全协议矩阵再次通过 | 新 server info、probe report |
| RN-10 | 真实断网负对照 | 删除 B 工作负载命名空间默认路由，定时自恢复 | 5 秒窗口内 frame 必须失败 | 失败 probe（预期失败） |
| RN-11 | 断网恢复 | 路由自动恢复，等待 grace period | frame/reconnect/blobs/gossip/docs 全部通过 | 恢复 probe report |

任何场景失败即 nightly 失败。没有 RN-10 的负对照，就不能把“恢复后成功”认定为断网恢复，因为故障注入可能根本未生效。

## 节点准备

1. 在 relay 主机部署受 TLS 保护的 `iroh-relay`，并开放 relay 所需 TCP/UDP 端口。
2. 节点 A 开放 `advertise_direct` 对应 UDP 端口。
3. 节点 B 建立 `adnet-nat` 网络命名空间：允许出站、做 SNAT/MASQUERADE、不配置入站端口映射。
4. 为验收 SSH 用户配置最小 `sudoers`：仅允许 `ip netns exec adnet-nat`、relay 健康检查和专用故障脚本。
5. 故障脚本必须先安排自动恢复，再删除工作负载默认路由；即使控制器崩溃，也必须在 `ADNET_FAULT_SECONDS` 后恢复。

故障脚本的安全契约如下（实现方式可用 `systemd-run` 或 `at`）：

```bash
#!/usr/bin/env bash
set -euo pipefail
seconds="${1:?duration required}"
# 先安排不可取消的恢复任务，再注入故障。
systemd-run --quiet --collect --on-active="${seconds}s" \
  /usr/local/sbin/adnet-acceptance-online
ip -n adnet-nat route del default
```

`adnet-acceptance-online` 应幂等恢复固定默认路由。不要让该脚本操作宿主机默认路由或 SSH 防火墙。

## CI secrets

在受保护的 GitHub Environment `real-network-nightly` 中配置：

- `ADNET_NETWORK_SSH_KEY`：专用、无交互密码、权限受限的 Ed25519 私钥；
- `ADNET_NETWORK_KNOWN_HOSTS`：由离线可信渠道固定的三台主机 host keys；
- `ADNET_NETWORK_INVENTORY_B64`：实际 inventory JSON 的 base64，结构参见 `ops/real-network.inventory.example.json`。

禁止运行时 `ssh-keyscan`，避免 CI 首次连接时信任被劫持的主机。inventory 不应保存私钥、token 或 relay 管理凭据。

## 本地/手工触发

先构建 Linux 目标机可运行的二进制，再执行：

```bash
cargo build --locked --release -p adnet-node --features iroh --example network_acceptance
python3 scripts/real-network-acceptance.py \
  --inventory /secure/path/inventory.json \
  --binary target/release/examples/network_acceptance \
  --output-dir artifacts/real-network
```

不同架构或 libc 的远端节点应使用对应交叉编译目标，或将 GitHub runner 固定到与两节点相同的平台。

## 证据与保留

每次运行生成：

- `summary.json`：整轮结论与各 case 证据；
- `results.jsonl`：便于日志平台逐行采集；
- `junit.xml`：GitHub checks 展示。

workflow 无论成功失败都上传证据，默认保留 30 天。探针把结构化报告写到 stdout、运行日志写到 stderr，避免日志污染 JSON 解析。
