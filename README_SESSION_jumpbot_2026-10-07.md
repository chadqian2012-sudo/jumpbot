# 跳一跳自动脚本 · 会话交接文档（给后续模型读）

> **生成时间**：2026-10-07（Asia/Shanghai）
> **文件名含义**：`README_SESSION_jumpbot_2026-10-07.md` —— 本会话（jumpbot 项目）的交接快照。
> **用途**：把多天会话里产生的**事实、结论、被证伪的假设、当前产物状态、踩过的坑**固化下来，
> 让另一个模型（或同一模型的不同配置）**不必重读全部历史**就能继续接手。
>
> **建议读法**：先看 §1 现状快照 + §2 目标口径，需要判断"某件事要不要做"时查 §4 结论库 与 §5 被证伪清单，
> 动手前过一遍 §6 陷阱。**§7 是命令速查**，§9 是压缩版时间线。

---

## 1. 现状快照（以磁盘与远端实测为准，非记忆）

### 2026-10-08：暂停/手动启动与新驱动检查（尚未发布）

当前源码已加入 `run_control.py` 的独立暂停/停止状态和原生 RegisterHotKey，
默认 Alt+P、无限等待用户手动打开窗口、3 秒启动/恢复倒计时。暂停时不发新输入，
已持有的按压正常释放；丢失画面的落地核对记录 unknown、不判死亡。
恢复清掉旧目标/帧/位置先验并重同步分数；暂停和恢复准备不计有效运行时间。
面板增加暂停状态、热键、暂停累计、最近100跳及覆盖率。
主 README 顶部有最新行为、配置、验收入口和证据边界。

新 NVIDIA 驱动 617.42 已实测：CuPy14.2、实际CUDA12.9、驱动API13.4，
源码与1.1旧冻结包 GPU/OpenCL 自检通过。500/1000/2000 CUDA 合成帧压测零差异、
设备内存稳定；515历史帧/35边界/两GPU全颜色HSV/回退通过。
23项控制测试通过，其中3500跳模拟长跑不是实机成绩；576帧回归和12跳控制重放通过。
新日志的 HUD note 是判定真值，离线报告不能跨暂停用人工增分重建命中。

2026-10-08 21:46 开始首轮CPU SIMD实机验收，记录在
`build/acceptance-20261008-cpu/` 与同名前缀 console.log。软件自己的探测确认窗口
553×1050；电脑操作辅助组件初始化仍失败，所以未验证人工跨应用/关窗/移窗场景。
22:02后用户确认已暂停，并明确要求停止升级轮次、精简代码、打包发布。
实际记录379跳：日志落地379/379、HUD命中355/367=96.73%、覆盖96.83%，
命中95%Wilson区间94.37%~98.12%。末段有读分异常与分数重置，不声称正式三轮验收通过。
用户按F8后未观察到退出事件，最终取消由本会话启动的测试会话；暂停前鼠标正常释放，
但不能称F8物理测试通过。取消未生成run-evidence.json，原始CSV/input-audit/control记录保留。
最终精简消除重复启动前台/窗口守卫、合并统计写入；CSV中心命中改用HUD真值，
像素判据移至pixel_center_hit；热键提示使用实际配置，面板处理停止状态。
后续以发布分支 `release/windows-pause-v1.2` / `D:\Codex Workspace\jumpbot-release-v1.2` 为候选，
从远端master c73ca708开始移植已完成CUDA/OpenCL与暂停改动；原目录不重置。
不要声称Release已完成，先查远端PR/CI/附件；不调k/gamma/瞄准门限凑成绩，不上传完整运行目录。

### 2026-10-07 续接更新：CUDA 优化

**最新续接：跨设备加速/安装包 1.1**。已正式加入 PyOpenCL 整数融合内核，
在 NVIDIA RTX 3070 Ti Laptop 与 Intel Iris Xe 上验证；AMD 无本机实测。
CPU SIMD 掩码 14.31→10.91ms（-23.8%），Intel OpenCL 13.05→4.13ms（对照为原 SIMD）。
5 个后端在 515 张干净帧上掩码/棋子/目标相同，35 组边界通过，两款 OpenCL GPU
各自穷举 1677 万种颜色与 OpenCV 相同。缺库/禁用/错误/设备不匹配回退通过。
自检、576 帧回归、自动/Intel/CPU 各 12 跳重放通过。`auto` 会比较 CPU/CUDA/OpenCL，
OpenCL 多卡核对并实测选卡，驱动首次编译做隔离预检；CPU 线程数 0=自动选 1~4。
交付目录为 `dist-accelerated/jumpbot` 和 `dist-portable/jumpbot`，
安装包为 `installer-accelerated/jumpbot-setup-1.1-full.exe` 与
`installer-portable/jumpbot-setup-1.1-portable.exe`，旧产物保留，安装时保留已有配置。
最终统一交付在 `release/1.1/`：完整/轻量两份安装包（124.25 / 33.92 MiB）和
两份免安装 ZIP（200.14 / 49.52 MiB），附交付说明与 SHA256SUMS。
ZIP 已解压到独立 build 验证目录，完整软件真实 CUDA/OpenCL 掩码零差异，
轻量软件禁 CUDA 后真实 Intel OpenCL 掩码零差异，自检均通过。
安装包编译日志确认同时包含根配置和 `_internal/config.json`；根配置仅首次安装时写入。
两份安装包版本资源为 1.1.0.0，未做代码签名；未执行覆盖旧软件的安装/卸载。
`.gitignore` 的安装产物规则已限定根目录，避免把 `tools/installer/` 的构建源码一并忽略。
代码/数据详见主 README 最上方、`tools/bench_backends.py` 与 `build/backends_benchmark.json`。
下段 CUDA 是前一阶段快照；本次同样未实机长跑，也未提交/上传远端。

后续会话已将 CUDA 掩码改为单个融合 `RawKernel`，包含与 OpenCV 整数规则相同的 BGR→HSV。
同进程交错 A/B（旧提交 `fdfcd9b`）掩码 3.866→1.920ms（2.01×），完整 detect
15.555→12.693ms（-18.4%）。515 张干净历史帧的掩码/棋子/目标零差异，75 个合成边界通过，
1677 万种颜色 HSV 穷举零差异，1000 帧设备内存稳定，异常回退/非默认流检查通过。
三条门禁均通过：576 帧回归成功数仍 492，读分 20/20、界面 5/5，重放 12/12。
新增 `tools/bench_cuda.py`，详细证据见主 README 开头和 `build/cuda_benchmark.json`。
优化版独立打包到 `dist-cuda/jumpbot/`，旧 `dist/jumpbot/` 及其日志保留。
打包版最终自检真实 GPU 掩码 1.79ms，GPU vs SIMD 差异 0；用 `run_long.bat` 启动。
下方 Git/产物/性能数据为原交接快照；本次改动尚未提交或上传远端。
当前默认仍为 `auto`，k=2.61 / gamma=0 等精度配置未改；本次未进行新的实机长跑验收。

### 1.1 文件与目录

| 路径 | 大小 | 说明 |
| --- | --- | --- |
| `D:\Codex Workspace\jumpbot\jumpbot.py` | 193.7 KB | **单文件主程序**（win32 层 / capture / vision / control / 日志都在里面） |
| `config.json` | 3.0 KB | 全部可调参数（含窗口尺寸守卫、白点门限、轮询节奏） |
| `README.md` | 72.9 KB | **完整实验记录**（每一项改动的证据链、量化结论、已知边界）← 本文件是它的"索引 + 交接口" |
| `run.bat` | 0.5 KB | 源码方式启动（走 `.venv`） |
| `tools\` | 0.3 MB / 37 文件 | 回归门禁、A/B 统计、打包脚本、复盘工具（见 §7） |
| `templates\` | 13 个 PNG | 棋子模板 + 0-9 数字模板 + 开始/重开界面模板 |
| `dist\jumpbot\` | 452.2 MB / 3005 文件 | PyInstaller onedir 产物（**含一次真实运行产生的 `logs/`、`debug/`**，安装包会排除） |
| `installer\jumpbot-setup-1.0.exe` | **122.7 MB** | Inno Setup 单文件安装包 |
| `logs\` | 82 个 `run_*.csv`（目录共 102 个文件，含控制台日志） | 累计约 **19,335 跳**（含调试轮）的逐跳记录 |
| `debug\` | 192.7 MB / 2866 文件 | 现场帧：失手 `miss/`、死亡 `dead_*`、识别失败 `fail_*`、调试图 `run_*/` |

### 1.2 运行环境

| 项 | 值 |
| --- | --- |
| 虚拟环境 | `D:\Codex Workspace\.venv` |
| Python | 3.12.14 |
| numpy / opencv / mss | 2.5.3 / 4.14.0 / 10.2.0 |
| GPU 加速 | **CuPy 14.2 + CUDA 12 运行时**（RTX 3070 Ti Laptop），`masks_backend="auto"` |
| 掩码耗时实测 | **GPU 3~6ms** / CPU SIMD 11~17ms / 纯 CPU 18~25ms（GPU 与 SIMD 逐像素一致） |
| 安装包工具 | **Inno Setup 6.7.3**：`%LOCALAPPDATA%\Programs\Inno Setup 6\ISCC.exe` |
| 显示器 | 2560×1440 @150% 缩放；游戏窗口客户区必须 **553×1050** |

### 1.3 Git 与远端

| 项 | 状态 |
| --- | --- |
| 本地仓库 | `D:\Codex Workspace\jumpbot`，HEAD = **`d6e60f1`**（4 个提交：初始 → 停止键 → 尺寸守卫 → 安装包） |
| 远端 | `https://github.com/chadqian2012-sudo/jumpbot`（**私有**，默认分支 `master`） |
| 远端 master | **`794bf41d`**（内容与本地一致，但 **SHA 不同**） |
| Release | `v1.0`，两个附件：`jumpbot-setup-1.0.exe`(122.7MB) + `jumpbot-win64-v1.0.zip`(198.1MB)，均 `uploaded` |

> ⚠️ **本地与远端历史分叉**：`github.com`（git 主机）在本机不可达（curl 都是 000/超时），
> 而 `api.github.com`、`uploads.github.com` 正常，所以远端那 3 个提交是**用 REST API 的 Git Data 接口**
> （blob → tree → commit → 更新 ref）打上去的，SHA 与本地不同但**内容一致**。
> 2026-10-08 发布约束：不能按旧建议 reset 现有目录来“对齐”。先核对两边内容，
> 保留本地加速改动，从远端 master 建发布分支移植已审查变化；不强推、不重置工作目录。

### 1.4 当前生效配置（`config.json` 关键项）

```
k = 2.61            k0 = 2.61          b = 0.0            gamma = 0.0（冻结）
aim_center_mode = estimate（几何模式已实现但默认关，见 §5）
masks_backend = auto   masks_simd = true
settle_poll_ms = 20.0  acquire_poll_ms = 40.0  score_reread_first_ms = 100.0
window_ref_width = 553  window_ref_height = 1050  window_size_policy = auto（尺寸守卫）
center_dot_degenerate_ratio = 0.12   center_dot_degenerate_radius_px = 120.0
low_ratio_gate = 0.45  low_ratio_trim_ratio = 0.0（实验开关，默认关）
press_point = [0.5, 0.72]           restart_point = [0.621, 0.761]
score_roi = [0.08, 0.13, 0.8, 0.24] center_hit_tolerance_px = 12.0
max_jumps = 5000                    max_runtime_s = 25200（7 小时）
```

---

## 2. 项目是什么 / 用户要什么

**项目**：微信小游戏「跳一跳」的屏幕识别自动脚本（Windows）。
截取游戏窗口画面 → OpenCV 识别棋子与目标方块 → 计算按压时长 → 模拟鼠标按住/松开 →
用落点误差在线修正 → 死亡自动重开 → 长跑守护。

**用户明确过的硬要求（按时间顺序）**

1. 能长时间跑、**不摔**、**尽可能命中中心**（"在不摔、长时间运行的情况下尽可能追求命中中心"）。
2. 语言不限（曾问"能不能换 C++"）——实测瓶颈不在语言，继续用 Python 单文件。
3. 以 **500 跳为一轮**作为初步验收单元，之后逐步加跳数（500 → 1000 → 2000 → 5000 → …）。
4. **只向游戏窗口客户区发送输入，窗口外绝不点击**（曾因离线基准误点别的窗口，已加两道安全闸）。
5. 运行时要有实时状态面板（PID / 状态 / 分数 / 实时命中率，**0.5 秒刷新**）。
6. 要有**命令行窗口里能按的停止键**（已加：`q`/`Q`/`Esc`）。
7. 要能**打包成软件**；进一步要**单文件 exe 安装包**（已完成，含开始菜单/卸载）。
8. 游戏窗口尺寸变了导致识别不了 → 要求"改回原来的大小"（已完成，并加了自动尺寸守卫）。
9. 把项目**上传 GitHub**（已完成，私有仓库 + Release 附件）。
10. 本文件：把会话内容导出成交接 README，供**别的模型在不同配置下续接**。

---

## 3. 系统怎么工作（架构速览）

**窗口层**

- 启动即 `SetProcessDpiAwareness(2)`，全程用**物理像素**（本机 150% 缩放 → 2560×1440）。
- 按标题正则 `^跳一跳$` + 进程名 `WeChatAppEx` 定位窗口；每轮重读客户区，支持移动/缩放。
- **尺寸守卫**：客户区不是 553×1050 就自动调回（启动进游戏前 + 每跳起跳前各一次；绝不在飞行途中动窗口）。

**识别层**（`Vision`）

1. `masks()`：逐行估计背景色，按"比背景更暗 / 色相不同 / 近纯白"分割出方块与棋子；
   后端可切 GPU(CuPy) / CPU SIMD，两者**逐像素一致**。
2. `find_piece()`：下方 ROI 连通域 + 模板校验 + 上一帧位置先验；锚点取包围盒**底边中点**（落脚点）。
3. `find_target()`：候选筛"在棋子上方 + 沿轴距离合理 + 横向比达标"；**中心白点**（游戏自带亮白小圆点）
   若找到就替代顶面估计中心；极端退化候选会把白点搜索半径放大到 120px（见 §4）。
4. `read_score()`：从 HUD 裁剪区用 0-9 数字模板 `matchTemplate` 定点读分（不引入 OCR 库）。

**控制层**

- 跳跃方向被游戏锁死在等距两轴上 → 按压时长按**沿轴投影距离**算：`hold_ms = k * along`（k=2.61 ms/px）。
- 前台模式用 `SendInput`（按下前先用 `WindowFromPoint` 确认按压点在目标窗口上，否则**不发键**）；
  后台模式用 `PostMessage`（成功率未验证，默认关）。

**落地确认**

- `wait_landing()`：连续 3 帧"画面稳定 + 棋子锚点不动 + 脚踩方块"判落地。
- 超时（2.6s）**不等于死亡**：若分数增长则按落地处理（详见 §4 的误判死亡修复）。

**长跑守护**

- `--daemon`：一轮结束后自动等窗口回来再开下一轮（窗口消失不是错误，是等待信号）；F8/控制台 `q` 退出整个 daemon。
- 单跳异常不终止整轮；看门狗 90 秒无进展写日志 + 状态；死亡后 `ensure_playing` 只在**匹配到开始/重开界面模板**时才点击。

---

## 4. 已验证的结论（可直接采信，均附证据）

| 结论 | 证据（可复现） |
| --- | --- |
| **k=2.61 已在最优工作点，不存在可修的系统偏置** | 700 跳偏置扫描（`--bias-sweep`，7 档×100 跳）：命中率峰在 0%（96.8%），±2% 掉到 95%/51%，±4% 掉到 54%/12.5%；**全档位落地率 100%**，说明跌的是真未中而非摔死 |
| **落点误差标准差只有 2~4px** | 由上面容差曲线反推（50% 命中点约 ±2% ≈ ±5px）；这也解释了"误差列看着很糟（±28px）却有 97% 命中"的矛盾 |
| 精度 ≈ 当前方案天花板 | 曲线两侧都是断崖而我们站在峰顶；剩余 ~2~3% 是误差分布尾部，不是偏置 |
| **误判死亡 = 0（修复后）** | 历史 103 次"死亡"里，可判定的 35 次中 **14 次是误判**（40%）；根因是"分数增长即判落地"的兜底被 `piece2 is not None` 挡住。修复后 `tools/report.py` 的 `⚠ 误判死亡` 一直为空 |
| 落地率 99.9~100% | 单轮 500 跳落地 500（100%）、482 跳落地 482（100%）；82 个日志里所有自然结束的轮次都是"达到最大跳跃次数"，**无意外停止** |
| 真实命中率 96.8~98.1% | 权威口径 = **读分判连击**（delta==1 普通落点；delta==min(2×连击,32) 命中中心）。最终配置最好一轮 98.12% |
| 无内存/显存泄漏 | 2000 次 `detect()` 压测：RSS 341→352MB 且趋于平台；CuPy 池 `used_bytes` 恒 0、`total` 恒 21.6MB |
| 磁盘不是风险 | D: 可用 447GB；`debug/` 每 5000 跳约增长 20~30MB |
| 系统负载不影响精度 | 最慢 10% 的跳反而 99.0%（比快的更高）；按压时长下发精度 p90=0.47ms |
| 落地确认内层循环可省 50ms/跳 | 交错 A/B（同条件交替跑新旧路径各 3 轮）：279ms → 230ms；另有 122 帧逐位比对零差异 |
| 极端退化块是**罕见但几乎必失**的短板 | 顶宽/连通域宽 <0.10 的候选命中率仅 25~36%（19/10459 跳 = 0.18%）；机制=连通域把相邻方块与阴影并成一大团 |
| 该短板**已部分修复** | 给退化候选把白点搜索半径从 55px 放大到 120px：16 个现场样本里白点可找回数 3→6；致命那次（13:58 摔掉 17015 分）瞄点从 (85,491) 校正到 (187,507)。**492 帧正常帧逐帧 A/B 零变化**（改动是外科手术式） |

---

## 5. 被证伪的假设（**别再重复走这些路**）

| 假设 | 为什么被否 |
| --- | --- |
| 掩码法落点误差（CSV 的 `error_px`）可用于调参 | 噪声 σ≈28px、**符号都会反**；同一批未命中样本里均值 −2.3px，而白点真值是 +23px |
| **"落地帧中心白点"可以当落点真值** | 35 个样本里 **12 行测量 >40px 却仍被游戏判为"命中中心"**。机制：落地瞬间棋子正好站在白点上、把它挡住了，检测器抓到的是别的高光/边界。现在白点值只写在 `err_dot_px` 列、**不再覆盖** `error_px` |
| 几何瞄点（`aim_center_mode=geometric`）更准 | 同码 1000 vs 1000 实机 A/B：**96.88% vs 97.30%，p=0.583 不显著**；二手指标也略偏 estimate。而且离线那套"瞄点误差减半"的证据只覆盖**有白点的帧**，生产在这些帧上本来就用白点 → 不能外推。**默认已回到 estimate** |
| "粘连块（ratio<0.45）是命中率短板" | 全量 10459 样本分层：<0.45 各类命中率 90.7~94.5%，与主体（0.6~0.8 为 94.4%）**没有实质差别**；历史那句 91.9% 是旧版本/小样本 |
| "短跳会有 +16px 系统过冲" | 是**测量假象**（棋子贴邻时"最宽连续段"锁到相邻块）；且 +16px 与 97.5% 的真实命中率不可能同时成立 |
| `--calibrate` 开环标定链可用 | 它需要的"可信落点误差"至今不存在（两路测量都被证伪）→ `gamma` 继续为 0 是对的，`transfer_a/c` 保持 0 |
| 换 C++ 能提速 | 瓶颈不在语言：单跳 2s 中按压 570ms + **游戏飞行动画 1238ms** 是游戏决定的，我们可压的只有抓帧识别那 135ms |
| 拖动窗口边框能"大概对齐" | 差几个像素阈值照样失配（绝对像素阈值：中心白点面积 14~260、最小方块宽 120px、落点安定容差 3px）→ 必须按"外框=客户区+边框"精确 SetWindowPos |

---

## 6. 陷阱与教训（踩过的坑，动手前必读）

1. **DPI**：不在进程里调 `SetProcessDpiAwareness(2)`，`GetWindowRect/ClientToScreen` 会返回**逻辑像素**，
   表现为"窗口明明 553×1050 却读成 368×700"，抓帧抓错位置（曾据此误判"窗口尺寸不对"）。
   探针脚本 `import jumpbot` **不会**自动设 DPI（`make_dpi_aware()` 只在 `main()` 里调），要手动调一次。
2. **强杀进程会跳死一局**：bot 正在按压时被 `Stop-Process` 杀掉，鼠标键状态/当局都会坏掉
   （实测分数从 36370 掉回新局）。要停就用 **F8**、**控制台 `q`**，或等 `debug/status.json` 的
   `state == "acquiring"`（那一刻没有按住鼠标）再停。合成 F8（`keybd_event`）**本机实测无效**。
3. **`debug/run_*/jump_NNN_before.png` / `_after.png` 是标注图**（`annotate()` 画过红/绿框与十字，
   实测含 3013 个纯绿、673 个纯红像素），**只有 `jump_NNN_raw.png` / `_raw_after.png` 是原始帧**。
   曾用它做数据集导致"误差 p90 41.66px / >12px 15.7% / 白点可用率 14.2%"全部偏大并作废。
   `tools/aim_error.py` 现在按文件名 + 纯红/纯绿像素计数双重剔除。
4. **`psapi.GetProcessMemoryInfo` 不设 `argtypes/restype` 会静默返回 0** → 得到"RSS 0.0 MB"假数据，
   差一点把内存泄漏结论写反。
5. **`.ps1` 必须纯 ASCII**：Windows PowerShell 5.1 按 ANSI 读文件，中文注释会破坏语法。
   **`.iss` 必须 UTF-8 带 BOM**：否则 Inno Setup 按 ANSI 读，中文全是乱码。
   提交信息里带引号时不要直接塞进命令，写成文件用 `git commit -F <file>`。
6. **打包体积陷阱**：本机装了 CUDA Toolkit 后，PyInstaller 会把 `cublasLt(507MB)/cusparse(274MB)/
   cufft(264MB)/cusolver(132MB)/cublas(100MB)` 等 **11 个 CUDA 数学库**一起收进包，
   `dist` 从 364MB 涨到 **1739MB**。`tools/jumpbot.spec` 已显式剔除（保留 `cudart64_12`、
   `nvrtc64_120_0`、`nvrtc-builtins`、`nvJitLink`）→ 回到 400MB。
7. **`gh` CLI 在本机对 `api.github.com` TLS 握手失败**（`schannel: server closed abruptly`），
   但 `curl` 访问同一域名正常 → 建仓/提交/上传附件改用 **`curl` + `gh auth token`** 直调 REST API。
   `github.com`（git 主机）当前完全不可达，所以 `git push/fetch` 都不通。
8. **安装包不要打包运行数据**：跑过之后 `dist\jumpbot\` 会多出 `logs/`、`debug/`
   （实测 debug 能到 49MB），`.iss` 里已用 `Excludes` 排除。
9. **每轮重读客户区**：bot 每跳都重读窗口位置/尺寸（支持窗口移动），但**尺寸突变会让绝对像素阈值失配**，
   所以加了尺寸守卫（§4）。
10. **离线实验也会被"数据来源"污染**：任何回归/评估前先确认喂进去的是**原始帧**、且用的是**当前代码**；
    `tools/regress.py` 已有"镜头过渡瞬时帧"分流（那种帧画面里没有位于棋子上方的合格候选，无目标属正常）。

---

## 7. 常用命令速查

```powershell
$py = "D:\Codex Workspace\.venv\Scripts\python.exe"
$root = "D:\Codex Workspace\jumpbot"

# —— 跑 ——
& $py "$root\jumpbot.py" --max-jumps 500 --daemon        # 前台长跑（daemon 自动续轮）
& $py "$root\jumpbot.py" --dry-run --samples 3           # 只识别不发输入（安全演示）
& $py "$root\jumpbot.py" --probe                         # 抓一帧 + 定位窗口（会顺带校正窗口尺寸）
# 停止：控制台按 q / Q / Esc，或全局 F8，或 Ctrl+C

# —— 自检与门禁（改代码后必跑）——
& $py "$root\jumpbot.py" --selftest                      # 依赖/模板/GPU/掩码一致性
& $py "$root\tools\regress.py"                           # 576 帧离线回归 + 关键帧闸门（读分 20/20、界面 5/5）
& $py "$root\tools\replay_test.py" --max 12              # 控制逻辑重放（落地判定 / 起跳前一致性）
& $py "$root\tools\check_globals.py"                     # 静态扫"给全局赋值漏写 global"（曾抓到 daemon 崩溃 bug）

# —— 复盘与实验 ——
& $py "$root\tools\report.py"                            # 单轮复盘：落地率/真实命中率/最长连击/误判死亡
& $py "$root\tools\report.py" logs\run_xxx.csv --json
& $py "$root\tools\cadence_report.py" logs\run_a.csv     # 每跳节奏拆分（抓帧/落地/读分）
& $py "$root\tools\ab_hitrate.py" --a f1,f2 --b f3,f4 --label-a A --label-b B   # 命中率 A/B + 显著性
& $py "$root\jumpbot.py" --bias-sweep --bias-levels 0,2,-2,4,-4,6,-6 --bias-per-level 100
& $py "$root\tools\bias_report.py" --glob                # 偏置扫描结果（峰在哪）
& $py "$root\tools\aim_error.py" --mode geometric        # 瞄点误差数据集（自动剔除标注图）
& $py "$root\tools\inspect_frame.py" debug\xxx.png       # 单帧候选诊断（列出所有候选 + 卡在哪条筛选）

# —— 打包 ——
& powershell -ExecutionPolicy Bypass -File "$root\tools\build_app.ps1"            # 绿色版 -> dist\jumpbot（~400MB）
& powershell -ExecutionPolicy Bypass -File "$root\tools\installer\build_installer.ps1"  # 安装包 -> installer\jumpbot-setup-1.0.exe
# 安装包依赖 Inno Setup 6：winget install JRSoftware.InnoSetup
```

**CSV 列**（`logs/run_*.csv`）：
`index, timestamp, k_before, dist_px, hold_ms, k_after, error_px, result, score, note, achieved_px, center_hit,`
`t_acquire_ms, t_landing_ms, t_score_ms, t_loop_ms, err_src, err_dot_px, aim_dot, bias_pct`

- `result`：`landed` / `dead` / `stopped`
- `score` 与 `note` 里的 `[center xN]` / `[miss]` / `[score基准]` 是**权威判据**（读分反推连击）
- `err_src`：`mask`（掩码法，噪声大） / `flow` / 空；`err_dot_px`：落地白点值（**非真值，仅参考**）
- `aim_dot`：本跳瞄点是否用了游戏中心白点；`bias_pct`：偏置扫描档位（0=未启用）

---

## 8. 已知边界与未做的事

1. **极端退化块（ratio<0.10，占 0.18%）**：命中率 25~36%。已修"有白点的 6/16"；剩下 10/16
   **画面里根本没有中心白点**，要救只能"按颜色/亮度把粘连连通域拆开"——高风险 CV 改动，
   **收益上限约 0.1 个百分点**，故决定不做。
2. **精度已到天花板**：偏置扫描证明工作点最优；剩余 ~2% 未命中是误差分布尾部。
3. **`gamma` 保持 0**：两路误差测量都不可信，自适应无从下手；`transfer_a/c` 保持 0。
4. **微信小游戏改版即失效**：渲染/配色/UI 模板一变，需要重采 `templates/` 与相关阈值（README 有重采入口）。
5. **本地/远端 git 历史分叉**（见 §1.3），恢复 github.com 后对齐一次。
6. **待办（若想继续推进）**：① 用当前配置跑 5000 跳复核（落地率 + 误判死亡=0）；
   ② 若愿意承担风险，再做退化块拆分实验（先离线量化，再实机 A/B）。

---

## 9. 会话时间线（压缩版）

**阶段一：从零到能跑（9/30）**

- 环境落地（Codex 内置 Python 3.12 + venv + opencv/mss/numpy），窗口定位 / DPI / 抓帧 / 掩码识别 / 按压控制跑通。
- 建立离线回归（`regress.py`）、读分模板、死亡现场留存、状态面板雏形。

**阶段二：精度与稳定性打磨（10/01 白天）**

- 误判死亡、卡死、影子误检等根因逐个修；`--daemon` 长跑模式；节奏列（抓帧/落地/读分）埋点。
- 实机成绩：500 跳 97.9%、1000 跳 97.8%、2000 跳 97.6%、**单条命 3972 跳 / 130071 分**。

**阶段三：优化专项（10/01 傍晚 ~ 夜间）**

- GPU(CuPy) 掩码、`masks_backend=auto`（GPU 被占用自动退回 SIMD）、打包瘦身 479→364MB。
- **落地确认内层循环去掉无用 `find_target`**：交错 A/B 省 50ms/跳（279→230ms）。
- 精度路线：先量化"瞄点误差"，实现 `aim_center_mode=geometric`，随后**同码 1000 vs 1000 A/B 证否**（p=0.583）→ 默认回 `estimate`。
- **700 跳偏置扫描**：证明 k=2.61 已在最优工作点、容差仅 ±5px 量级、短边比长边危险。
- 误判死亡深挖：现场帧复现 + 逻辑回归，修掉"分数增长兜底被 `piece2` 挡住"。
- 资源审计：磁盘/内存/显存三项压测无泄漏。

**阶段四：交互与交付（10/04 ~ 10/07）**

- 命令行停止键 `q/Q/Esc` + Ctrl+C 优雅退出。
- **窗口尺寸守卫**：客户区偏离 553×1050 自动调回（实测 500×950 → 553×1050）。
- **exe 安装包**（Inno Setup，122.7MB）：每用户安装、开始菜单 5 个快捷方式、标准卸载、中文向导；
  实测静默装→自检全过→静默卸载全清。
- 上传 GitHub（私有仓库 + Release 双附件）；因 `github.com` 不可达，改用 REST API 提交。

**最终战绩（当前配置）**

| 轮次 | 跳数 | 死亡 | 落地率 | 真实命中率 | 备注 |
| --- | --- | --- | --- | --- | --- |
| 收尾轮 | 482 | 0 | **100%** | **98.12%** | 最长连击 **179**，单条命 13019 分 |
| 前一轮 | 500 | 0 | 100% | 97.39% | 误判死亡 0 |
| 历史最好 | 3972 | 1 | 99.97% | 97.01% | 单条命 130071 分（旧配置） |

---

## 10. 给接手模型的建议

1. **先跑 `--selftest` + `regress.py` + `replay_test.py`**：三条门禁全绿再谈改动。
2. **判据优先用"分数"**（读分反推连击），它是唯一权威信号；像素级误差列不可用于调参。
3. **改任何影响识别/控制的数值前，先离线 A/B**（`ab_vision.py` / `aim_error.py` / `regress.py`），
   再实机 A/B（`ab_hitrate.py`，注意 500 跳的标准误约 0.76%，小差异看不出来）。
4. **别在实机跑着的时候改代码**：运行中的进程已加载旧代码；要生效必须重启，而重启要用
   §6.2 的安全方式（否则会跳死一局）。
5. **文档优先写"证据 + 是否被证伪"**：本项目的价值一半在结论，一半在**否掉的假设**——
   那些是最容易重复踩的坑。
