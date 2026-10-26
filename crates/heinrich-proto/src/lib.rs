//! DLL ↔ server 的帧协议。
//!
//! ## 形态
//!
//! ```text
//! [ len: u32 ][ kind: u16 ][ payload ... ]
//!              └────────── len 覆盖这两部分 ──────────┘
//! ```
//!
//! 小端、定长前缀、无对齐要求。**不引 serde / bincode**：这个 crate 会被编进
//! 注入型 DLL（D8 只允许 `windows` + `heinrich-tip-id` + 本 crate），每个宿主进程
//! 都要多背一份；而协议只有 8 条消息，手写编解码比引依赖便宜。
//!
//! ## 为什么带长度前缀而不是定界符
//!
//! 候选词是任意 UTF-8（可能含换行、可能含 `\0` 之外的任何字节），定界符方案要么
//! 需要转义、要么要靠「读不到就等」——后者正是阻塞的来源。长度前缀让每一次
//! 读取的字节数都是确定的，客户端才能给整次往返套一个硬超时（D18）。
//!
//! ## 版本
//!
//! 握手靠 [`Frame::Hello`] / [`Frame::Welcome`] 里的 `proto`，不匹配由 server 回
//! [`Frame::Bye`]。改消息语义时必须同时递增 [`PROTO_VERSION`]。

use std::fmt;

/// 协议版本。与 `HeinrichTsfAbiVersion` 是两件事：那是 DLL 的导出契约，
/// 这是线上的字节契约。
///
/// - v1：M1.3 的首版（Hello / Query / Result / SetMode / Mode / Bye）
/// - v2：M2.4 加入 [`Frame::Record`]（上屏回报）。**只加了一条单向消息**，
///   旧客户端连上来会被 `Bye` 挡掉——版本号是对称的，宁可握手失败也不要
///   让 server 收到自己解释不了的帧。
/// - v3：M3.3 混输。`Welcome` 加 `features`（能力位），`Result` 加 `source` /
///   `primary_len`（判定语言与折叠分界）。三个字段都插在原有字段**之前**的固定
///   位置，所以字节布局变了、但读顺序仍是直的；旧 DLL 连上来会因为版本不符被
///   `Bye`，不会把 `source` 当成候选条数读出一堆乱码。
/// - v4：M3.4a 码提示。`Entry` 加 `code`（**追加在 `label` 之后**，前三个字段的
///   位置不动）。码由 server 决定给不给（`hint.mode` 与「这一段是怎么出来的」都在
///   server 侧），DLL 只负责「非空就画 chip」——这样提示规则只有一处实现。
/// - v5：M3.4b 键位图。新增 [`Frame::Roots`]（server → DLL，**连接建立后主动推一帧**）。
///   这是协议里第一条**不是应答**的帧，所以客户端读应答时必须跳过它
///   （`apps/windows/tsf/src/ipc.rs` 的 `exchange`），否则下一条请求会读到别人的
///   应答、整条管道错位一帧。
/// - v6：M3.5/D64 词中码提示。`Entry` 加 `hint_at`（**一个字节，追加在 `code`
///   之后**）：chip 前面有几个字符，`0` = 挂词尾（单段候选与通配的既有行为）。
///   多段组合（`hao'ktn` → 好吃）的 chip 必须贴着拼音段的字（`好[vbg]吃`），
///   挂词尾会被误读成整个词的五笔码。
/// - v7：M4c/D66 emoji 标签。`Entry` 加 `emoji`（**字符串，追加在 `hint_at` 之后**）：
///   词组候选右侧挂的那枚 emoji，空串 = 本次不挂。
///   这个字段只活了同一天：真机上跑起来才发现「装饰」不是要的东西——用户要的是
///   **emoji 自己作为候选**、选中它上屏 emoji。于是 v8 把它整个换掉，v7 没有随
///   任何一版分发出去。
/// - v8：M4c 语义修正。`Entry` 去掉 `emoji` 字符串，换成 `kind`（**一个字节，
///   位置不变**）：`0` = 普通词候选，`1` = emoji 候选。
///   emoji 候选的 `word` 就是那枚 emoji 本身，选中它原样上屏——不再是「词的装饰」，
///   而是「词的邻居」。上屏路径因此不需要任何 emoji 专用分支，客户端只要按 `kind`
///   决定两件事：用彩色字体画（`ENABLE_COLOR_FONT`），以及**不上报学习数据**
///   （否则 emoji 会被记成该编码的一个词）。
///
/// **v7 之前那次「v7 没有发生」**：D65 自定义词组最初打算加一条 `Reload` 帧让 CLI
/// 通知 server 重载词库，实现时发现 server 本来就有「按 mtime 热更新配置文件」的
/// 机制，把 `words.tsv` 挂进同一条路径即可——改完文件，下一次按键就生效，而按键
/// 本来就是看到候选的前提。那次没升版；M4c 的 emoji 字段才是真正的 v7，
/// 而它随即被 v8 的 `kind` 取代。
pub const PROTO_VERSION: u16 = 8;

/// 单帧体积上限。一个 `Result` 最多 32 个候选，正常远小于这个数；
/// 上限的作用是**不让一个坏掉的长度前缀把对端的内存吃光**。
pub const MAX_PAYLOAD: usize = 48 * 1024;

/// 一帧最多带多少个候选。DLL 只需要首个候选（M1.3），但协议一次给足 9 个，
/// 免得 M2 加候选窗时再改线格式。
pub const MAX_ENTRIES: usize = 32;

/// 一帧最多带多少个键位（[`Frame::Roots`]）。五笔只有 25 个字母键，
/// 上限给 32 是留一格余量、同时挡住坏帧。
pub const MAX_KEYS: usize = 32;

/// 候选来源（`Frame::Result::source`）。
pub mod source {
    /// 五笔码命中
    pub const WUBI: u8 = 0;
    /// 拼音命中（含混输判定为拼音的情况）
    pub const PINYIN: u8 = 1;
}

/// 能力位（`Frame::Welcome::features`）。
pub mod feature {
    /// 服务端加载了拼音词库，混输可用。
    ///
    /// DLL 看到这一位才敢做两件事：让 `z` 进缓冲区（拼音里 `zhuang` 的 `z` 是
    /// 普通字母），以及不再把组合串卡在 4 码以内（`zhuang` 有 6 个字母）。
    /// 没这一位时保持五笔的老行为——`z` 吃掉、超 4 码拒绝。
    pub const PINYIN: u8 = 0b0000_0001;
}

/// 中英模式。**由 server 持有**——DLL 每个宿主进程一份实例，模式若也存在实例里，
/// A 应用切了英文、B 应用仍是中文，用户会当成 bug。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Chinese,
    English,
}

impl Mode {
    pub const fn as_u8(self) -> u8 {
        match self {
            Mode::Chinese => 0,
            Mode::English => 1,
        }
    }

    pub const fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Mode::Chinese),
            1 => Some(Mode::English),
            _ => None,
        }
    }
}

/// 一个候选。`label` 是「全码 / 一级简码 / 二级简码 / 三级简码」这类给人看的标签，
/// 直接沿用引擎的文案，避免协议与引擎各维护一份枚举。
///
/// `code` 是这个词的**五笔码**（拼音段候选用来画码提示 chip，§6.3）。它**不一定**
/// 等于本次的查询串：用拼音查 `hao`，命中「好」时查询串是 `hao`、而码是 `vb`。
/// 空串表示「本次不提示」（server 已经按 `hint.mode` 与段来源裁决过了）。
///
/// `hint_at` 是 chip 的**词中位置**（D64）：`code` 非空时，chip 画在词的第
/// `hint_at` 个字符之后（`0` = 词尾）。多段组合 `hao'ktn` → 好吃 时 server 给
/// `hint_at = 1`、`code = vbg`，DLL 画成 `好[vbg]吃`；单段候选与通配恒为 `0`。
///
/// `kind` 是**这一条是什么**（v8）：普通词候选，还是 emoji 候选。
/// emoji 候选的 `word` 就是那枚 emoji，`code` / `label` / `hint_at` 全是空与 `0`；
/// 它由 server 合成（紧跟在那条有 emoji 绑定的词后面），`limit` 记账时算一条。
/// 是否给、给哪几枚，全在 server 裁决——DLL 只按 `kind` 决定画法与上报，
/// 与 `code` 同一条纪律，规则只有一处实现。
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Entry {
    pub word: String,
    pub label: String,
    pub code: String,
    pub hint_at: u8,
    pub kind: EntryKind,
}

/// 候选的种类。
///
/// 只有两种：多一种就要同时改 DLL 的绘制、上屏与上报三处，而这三处目前
/// 只关心「是不是 emoji」这一个问题。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EntryKind {
    /// 普通的词候选。
    Word,
    /// emoji 候选：`word` 是一枚 emoji，上屏即 emoji，且**不写学习数据**。
    Emoji,
}

impl EntryKind {
    pub fn as_u8(self) -> u8 {
        match self {
            EntryKind::Word => 0,
            EntryKind::Emoji => 1,
        }
    }

    pub fn from_u8(raw: u8) -> Option<Self> {
        match raw {
            0 => Some(EntryKind::Word),
            1 => Some(EntryKind::Emoji),
            _ => None,
        }
    }
}

/// 一个键位上的字根（[`Frame::Roots`] 的一条）。
///
/// 只带**画键帽需要的**四样东西。区位号与助记口诀不上线：键位图上不画它们，
/// 多传就是多一份要跟着 `roots.toml` 同步的字段。要显示助记再加字段，
/// 那时协议也得跟着升版——但现在没有任何一处需要它。
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ProtoKeyRoots {
    /// 字母键，`a`–`y`。线格式是一个字节（ASCII）。
    pub key: char,
    /// 一级简码字，画在键帽右下。
    pub primary: char,
    /// 主字根在前、变体在后，**每个 `char` 是一枚字根**（不是拆字结果）。
    pub roots: String,
    /// `roots` 前几枚是主字根，其余在键帽上淡一档。
    pub main_len: u8,
}

/// 一条消息。
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Frame {
    /// 客户端打招呼。server 用它同时充当「取当前模式」——重新发一次就是重新问一次。
    Hello { proto: u16, scheme_id: String },
    /// server 应答握手。
    Welcome {
        proto: u16,
        scheme_id: String,
        entry_count: u32,
        mode: Mode,
        /// 能力位标志，见 [`feature`]。DLL 靠它判断该不该放开拼音那套行为
        /// （`z` 进缓冲、组合串可以超过 4 码）——没有这个词，DLL 只能猜。
        features: u8,
    },
    /// 精确码查询。
    Query { code: String, limit: u8 },
    /// 查询结果。空 `entries` 表示「这个码没有词条」，与「查询失败」是两回事。
    Result {
        code: String,
        entries: Vec<Entry>,
        /// 判定来源：见 [`source`]。决定候选窗把它当五笔还是拼音（§6.2 的分区）
        source: u8,
        /// 前 N 条属于首屏（判定语言），其余是折叠区。`0` 表示「不分折叠」，
        /// 全部算首屏——DLL 在旧版本兼容路径下会看到这个值。
        primary_len: u16,
    },
    /// 改中英模式（单向，server 回一条 [`Frame::Mode`] 确认）。
    SetMode { mode: Mode },
    /// 当前模式。
    Mode { mode: Mode },
    /// 上屏回报：某个码上屏了哪个词。**单向**，server 不回帧。
    ///
    /// `at_unix` 是 DLL **提交那一刻**的时间，不是 server 收到的时间。两者通常只差
    /// 几毫秒，但 server 被强杀时 DLL 会把这批记录积压在进程里，等重连后一起冲上来
    /// ——那时用收到的时间算「距上次使用多久」会把一次刚发生的选择算成几分钟前的，
    /// 学习分的衰减就跟着错。时间戳由提交方给，这件事就没得商量。
    Record {
        code: String,
        word: String,
        at_unix: i64,
    },
    /// server 拒绝服务或要断开。
    Bye { reason: String },
    /// 字根表（§6.7 键位图）。**server 主动推**，不是任何请求的应答。
    ///
    /// 只在**每条连接的第一次握手成功后**发一次：数据是装配期就固定下来的，
    /// 一整条连接里不会变，没必要每次 `Hello` 都重发（`Hello` 还兼着「问当前
    /// 中英模式」的职责，会被 `Activate` 反复调用）。
    ///
    /// DLL 收到后缓存；没收到（老 server、或这一帧丢了）就一直不接管
    /// `Ctrl+Shift+N`——键位图画不出来时，那个键本来就该还给宿主。
    Roots { keys: Vec<ProtoKeyRoots> },
}

mod kind {
    pub const HELLO: u16 = 1;
    pub const WELCOME: u16 = 2;
    pub const QUERY: u16 = 3;
    pub const RESULT: u16 = 4;
    pub const SET_MODE: u16 = 5;
    pub const MODE: u16 = 6;
    pub const BYE: u16 = 7;
    pub const RECORD: u16 = 8;
    pub const FROOTS: u16 = 9;
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DecodeError {
    /// 字节不够（截断的帧）。
    Truncated,
    /// 未知的消息类型。
    BadKind(u16),
    /// 长度前缀超出 [`MAX_PAYLOAD`]。
    TooLarge(usize),
    BadMode(u8),
    /// 候选的种类字节不认识（v8）。
    BadEntryKind(u8),
    /// 帧尾有多余字节。
    Trailing(usize),
    TooManyEntries(usize),
    /// `Frame::Roots` 里的键位过多。
    TooManyKeys(usize),
    /// 键位的一级简码不是恰好一个字符。
    BadKeyPrimary,
    /// 主字根个数超过字根总数。
    BadKeyRoots,
    BadUtf8,
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DecodeError::Truncated => write!(f, "帧被截断"),
            DecodeError::BadKind(k) => write!(f, "未知消息类型 {k}"),
            DecodeError::TooLarge(n) => write!(f, "帧过大（{n} 字节）"),
            DecodeError::BadMode(m) => write!(f, "未知模式 {m}"),
            DecodeError::Trailing(n) => write!(f, "帧尾多出 {n} 字节"),
            DecodeError::TooManyEntries(n) => write!(f, "候选过多（{n} 个）"),
            DecodeError::TooManyKeys(n) => write!(f, "键位过多（{n} 个）"),
            DecodeError::BadKeyPrimary => write!(f, "键位的一级简码不是单个字符"),
            DecodeError::BadKeyRoots => write!(f, "主字根个数超过字根总数"),
            DecodeError::BadUtf8 => write!(f, "字符串不是合法 UTF-8"),
            DecodeError::BadEntryKind(raw) => write!(f, "候选种类字节 {raw} 不认识"),
        }
    }
}

impl std::error::Error for DecodeError {}

impl Frame {
    pub fn kind(&self) -> u16 {
        match self {
            Frame::Hello { .. } => kind::HELLO,
            Frame::Welcome { .. } => kind::WELCOME,
            Frame::Query { .. } => kind::QUERY,
            Frame::Result { .. } => kind::RESULT,
            Frame::SetMode { .. } => kind::SET_MODE,
            Frame::Mode { .. } => kind::MODE,
            Frame::Record { .. } => kind::RECORD,
            Frame::Roots { .. } => kind::FROOTS,
            Frame::Bye { .. } => kind::BYE,
        }
    }

    /// 编成完整的一帧（含长度前缀）。可直接整块写进管道。
    pub fn encode(&self) -> Vec<u8> {
        let mut body = Vec::with_capacity(64);
        body.extend_from_slice(&self.kind().to_le_bytes());
        match self {
            Frame::Hello { proto, scheme_id } => {
                body.extend_from_slice(&proto.to_le_bytes());
                put_str(&mut body, scheme_id);
            }
            Frame::Welcome {
                proto,
                scheme_id,
                entry_count,
                mode,
                features,
            } => {
                body.extend_from_slice(&proto.to_le_bytes());
                put_str(&mut body, scheme_id);
                body.extend_from_slice(&entry_count.to_le_bytes());
                body.push(mode.as_u8());
                body.push(*features);
            }
            Frame::Query { code, limit } => {
                put_str(&mut body, code);
                body.push(*limit);
            }
            Frame::Result {
                code,
                entries,
                source,
                primary_len,
            } => {
                put_str(&mut body, code);
                body.push(*source);
                body.extend_from_slice(&primary_len.to_le_bytes());
                body.push(entries.len() as u8);
                for e in entries {
                    put_str(&mut body, &e.word);
                    put_str(&mut body, &e.label);
                    // v4 的追加字段：老 DLL 读到这里会剩一截尾巴，由
                    // `decode_body` 的「帧尾不许多字节」拦下——正是我们要的效果，
                    // 模糊兼容会把码当成下一条候选的词。
                    put_str(&mut body, &e.code);
                    // v6 的追加字段：chip 的词中位置（D64）。
                    body.push(e.hint_at);
                    // v8 的追加字段：候选种类（一个字节）。位置沿用 v7 的
                    // emoji 字段——字段换掉了，但尾巴的长度与顺序没动。
                    body.push(e.kind.as_u8());
                }
            }
            Frame::SetMode { mode } => body.push(mode.as_u8()),
            Frame::Mode { mode } => body.push(mode.as_u8()),
            Frame::Record {
                code,
                word,
                at_unix,
            } => {
                put_str(&mut body, code);
                put_str(&mut body, word);
                body.extend_from_slice(&at_unix.to_le_bytes());
            }
            Frame::Bye { reason } => put_str(&mut body, reason),
            Frame::Roots { keys } => {
                body.push(keys.len() as u8);
                for k in keys {
                    // 键位字母是 ASCII，一个字节足够；写成 `as u8` 而不是字符串，
                    // 25 个键就省下 25 个长度前缀。
                    body.push(k.key as u8);
                    put_str(&mut body, &k.primary.to_string());
                    body.push(k.main_len);
                    put_str(&mut body, &k.roots);
                }
            }
        }
        let mut out = Vec::with_capacity(body.len() + 4);
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        out.extend_from_slice(&body);
        out
    }

    /// 解一整帧（含长度前缀，且不允许帧尾有多余字节）。
    pub fn decode(bytes: &[u8]) -> Result<Self, DecodeError> {
        if bytes.len() < 4 {
            return Err(DecodeError::Truncated);
        }
        let len = body_len([bytes[0], bytes[1], bytes[2], bytes[3]])?;
        let rest = &bytes[4..];
        if rest.len() < len {
            return Err(DecodeError::Truncated);
        }
        let frame = decode_body(&rest[..len])?;
        let trailing = rest.len() - len;
        if trailing != 0 {
            return Err(DecodeError::Trailing(trailing));
        }
        Ok(frame)
    }
}

/// 解出长度前缀里声明的帧体长度（**不含** 4 字节前缀本身）。
///
/// 拆出来是因为收发两端都是「先读 4 字节、再按长度读帧体」，
/// 谁都不需要先攒一整帧。
pub fn body_len(header: [u8; 4]) -> Result<usize, DecodeError> {
    let len = u32::from_le_bytes(header) as usize;
    if len > MAX_PAYLOAD {
        return Err(DecodeError::TooLarge(len));
    }
    if len < 2 {
        // 至少要有 2 字节的消息类型。
        return Err(DecodeError::Truncated);
    }
    Ok(len)
}

fn decode_body(body: &[u8]) -> Result<Frame, DecodeError> {
    let mut r = Reader { buf: body, pos: 0 };
    let k = r.u16()?;
    let frame = match k {
        kind::HELLO => Frame::Hello {
            proto: r.u16()?,
            scheme_id: r.string()?,
        },
        kind::WELCOME => {
            let proto = r.u16()?;
            let scheme_id = r.string()?;
            let entry_count = r.u32()?;
            let mode = r.mode()?;
            let features = r.u8()?;
            Frame::Welcome {
                proto,
                scheme_id,
                entry_count,
                mode,
                features,
            }
        }
        kind::QUERY => Frame::Query {
            code: r.string()?,
            limit: r.u8()?,
        },
        kind::RESULT => {
            let code = r.string()?;
            let source = r.u8()?;
            let primary_len = r.u16()?;
            let n = r.u8()? as usize;
            if n > MAX_ENTRIES {
                return Err(DecodeError::TooManyEntries(n));
            }
            let mut entries = Vec::with_capacity(n);
            for _ in 0..n {
                entries.push(Entry {
                    word: r.string()?,
                    label: r.string()?,
                    code: r.string()?,
                    hint_at: r.u8()?,
                    kind: {
                        let raw = r.u8()?;
                        EntryKind::from_u8(raw).ok_or(DecodeError::BadEntryKind(raw))?
                    },
                });
            }
            Frame::Result {
                code,
                entries,
                source,
                primary_len,
            }
        }
        kind::SET_MODE => Frame::SetMode { mode: r.mode()? },
        kind::MODE => Frame::Mode { mode: r.mode()? },
        kind::RECORD => Frame::Record {
            code: r.string()?,
            word: r.string()?,
            at_unix: r.i64()?,
        },
        kind::BYE => Frame::Bye {
            reason: r.string()?,
        },
        kind::FROOTS => {
            let n = r.u8()? as usize;
            if n > MAX_KEYS {
                return Err(DecodeError::TooManyKeys(n));
            }
            let mut keys = Vec::with_capacity(n);
            for _ in 0..n {
                let key = r.u8()? as char;
                let primary = r.string()?;
                let mut pc = primary.chars();
                let primary = match (pc.next(), pc.next()) {
                    (Some(c), None) => c,
                    // 空串或多字符都当坏帧：键帽上一个格子只能放一个字。
                    _ => return Err(DecodeError::BadKeyPrimary),
                };
                let main_len = r.u8()?;
                let roots = r.string()?;
                if main_len as usize > roots.chars().count() {
                    return Err(DecodeError::BadKeyRoots);
                }
                keys.push(ProtoKeyRoots {
                    key,
                    primary,
                    roots,
                    main_len,
                });
            }
            Frame::Roots { keys }
        }
        other => return Err(DecodeError::BadKind(other)),
    };
    if r.pos != body.len() {
        return Err(DecodeError::Trailing(body.len() - r.pos));
    }
    Ok(frame)
}

fn put_str(out: &mut Vec<u8>, s: &str) {
    let b = s.as_bytes();
    // 协议里所有字符串都是短串（编码 ≤ 4 字节、词 ≤ 一行）。超长一律截断，
    // 而不是让 `as u16` 静默回绕——回绕会产出错位的帧，比截断难查得多。
    let n = b.len().min(u16::MAX as usize);
    out.extend_from_slice(&(n as u16).to_le_bytes());
    out.extend_from_slice(&b[..n]);
}

struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], DecodeError> {
        if self.pos + n > self.buf.len() {
            return Err(DecodeError::Truncated);
        }
        let s = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }

    fn u8(&mut self) -> Result<u8, DecodeError> {
        Ok(self.take(1)?[0])
    }

    /// 读一个模式字节，非法值原样带进错误里，便于对着日志查。
    fn mode(&mut self) -> Result<Mode, DecodeError> {
        let raw = self.u8()?;
        Mode::from_u8(raw).ok_or(DecodeError::BadMode(raw))
    }

    fn u16(&mut self) -> Result<u16, DecodeError> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    fn u32(&mut self) -> Result<u32, DecodeError> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn i64(&mut self) -> Result<i64, DecodeError> {
        let b = self.take(8)?;
        let mut a = [0u8; 8];
        a.copy_from_slice(b);
        Ok(i64::from_le_bytes(a))
    }

    fn string(&mut self) -> Result<String, DecodeError> {
        let n = self.u16()? as usize;
        let b = self.take(n)?;
        std::str::from_utf8(b)
            .map(str::to_owned)
            .map_err(|_| DecodeError::BadUtf8)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(f: Frame) {
        let bytes = f.encode();
        let back = Frame::decode(&bytes).expect("应能解回");
        assert_eq!(f, back);
    }

    #[test]
    fn every_frame_roundtrips() {
        roundtrip(Frame::Hello {
            proto: PROTO_VERSION,
            scheme_id: "wubi86".into(),
        });
        roundtrip(Frame::Welcome {
            proto: PROTO_VERSION,
            scheme_id: "wubi86".into(),
            entry_count: 136_239,
            mode: Mode::English,
            features: feature::PINYIN,
        });
        roundtrip(Frame::Query {
            code: "vbg".into(),
            limit: 9,
        });
        roundtrip(Frame::Result {
            code: "vbg".into(),
            source: source::WUBI,
            primary_len: 0,
            entries: vec![
                Entry {
                    word: "好".into(),
                    label: "全码".into(),
                    code: "vbg".into(),
                    hint_at: 0,
                    kind: EntryKind::Word,
                },
                Entry {
                    word: "好玩".into(),
                    label: "全码".into(),
                    code: "vbgf".into(),
                    hint_at: 0,
                    kind: EntryKind::Word,
                },
            ],
        });
        roundtrip(Frame::SetMode {
            mode: Mode::Chinese,
        });
        roundtrip(Frame::Mode {
            mode: Mode::English,
        });
        roundtrip(Frame::Record {
            code: "vbg".into(),
            word: "好".into(),
            at_unix: 1_766_000_000,
        });
        roundtrip(Frame::Bye {
            reason: "协议版本不匹配".into(),
        });
        roundtrip(Frame::Roots {
            keys: vec![
                ProtoKeyRoots {
                    key: 'v',
                    primary: '发',
                    roots: "女刀九臼山彐曰".into(),
                    main_len: 7,
                },
                ProtoKeyRoots {
                    key: 'g',
                    primary: '一',
                    roots: "王一五戋龶丰".into(),
                    main_len: 4,
                },
            ],
        });
    }

    /// 字根表这一帧的规模要可算：25 键、每键七八枚字根（8 枚已是 `roots.toml`
    /// 里最大的那一档）时也就 800 多字节。这条不是性能测试，是防止有人往
    /// `roots` 里塞整本字典。
    #[test]
    fn roots_frame_stays_small() {
        let keys: Vec<ProtoKeyRoots> = "abcdefghijklmnopqrstuvwxy"
            .chars()
            .map(|c| ProtoKeyRoots {
                key: c,
                primary: '一',
                roots: "女刀九臼山彐曰金".into(),
                main_len: 6,
            })
            .collect();
        let bytes = Frame::Roots { keys }.encode();
        assert!(bytes.len() < 1024, "字根表 {} 字节，超了", bytes.len());
    }

    /// 主字根个数超出字根总数是**坏帧**，不能画出来当「全是主字根」。
    #[test]
    fn roots_with_bad_main_len_are_rejected() {
        let mut body = Vec::new();
        body.extend_from_slice(&kind::FROOTS.to_le_bytes());
        body.push(1);
        body.push(b'g');
        put_str(&mut body, "一");
        body.push(9); // 只有 2 枚字根
        put_str(&mut body, "王一");
        let mut bytes = (body.len() as u32).to_le_bytes().to_vec();
        bytes.extend_from_slice(&body);
        assert_eq!(Frame::decode(&bytes), Err(DecodeError::BadKeyRoots));
    }

    /// 长度前缀必须覆盖「消息类型 + 载荷」，客户端靠它一次读准。
    #[test]
    fn length_prefix_covers_kind_and_payload() {
        let bytes = Frame::Query {
            code: "vbg".into(),
            limit: 1,
        }
        .encode();
        let len = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize;
        assert_eq!(len, bytes.len() - 4);
        assert_eq!(body_len([bytes[0], bytes[1], bytes[2], bytes[3]]), Ok(len));
    }

    #[test]
    fn truncated_frame_is_rejected() {
        let bytes = Frame::Query {
            code: "vbg".into(),
            limit: 1,
        }
        .encode();
        for cut in 0..bytes.len() {
            assert!(
                Frame::decode(&bytes[..cut]).is_err(),
                "截到 {cut} 字节时不该解出帧"
            );
        }
    }

    /// 帧尾多字节必须是错：否则管道里错位一格就再也对不上了。
    #[test]
    fn trailing_bytes_are_rejected() {
        let mut bytes = Frame::Mode {
            mode: Mode::Chinese,
        }
        .encode();
        bytes.push(0);
        assert_eq!(Frame::decode(&bytes), Err(DecodeError::Trailing(1)));
    }

    #[test]
    fn oversized_length_prefix_is_rejected() {
        let header = ((MAX_PAYLOAD + 1) as u32).to_le_bytes();
        assert_eq!(
            body_len(header),
            Err(DecodeError::TooLarge(MAX_PAYLOAD + 1))
        );
    }

    #[test]
    fn unknown_kind_is_rejected() {
        let mut bytes = vec![0u8; 6];
        let body = [0xEEu8, 0x00]; // kind = 0x00EE
        bytes[0..4].copy_from_slice(&(body.len() as u32).to_le_bytes());
        bytes[4..6].copy_from_slice(&body);
        assert_eq!(Frame::decode(&bytes), Err(DecodeError::BadKind(0x00EE)));
    }

    #[test]
    fn bad_mode_is_rejected() {
        let mut body = Vec::new();
        body.extend_from_slice(&kind::MODE.to_le_bytes());
        body.push(9);
        let mut bytes = (body.len() as u32).to_le_bytes().to_vec();
        bytes.extend_from_slice(&body);
        assert_eq!(Frame::decode(&bytes), Err(DecodeError::BadMode(9)));
    }

    /// 中文词必须原样往返（UTF-8 字节长度而不是字符数）。
    #[test]
    fn multibyte_words_survive() {
        let f = Frame::Result {
            code: "vbgf".into(),
            source: source::PINYIN,
            primary_len: 1,
            entries: vec![Entry {
                word: "好玩".into(),
                label: "三级简码".into(),
                code: "vbgf".into(),
                hint_at: 0,
                kind: EntryKind::Word,
            }],
        };
        let bytes = f.encode();
        let back = Frame::decode(&bytes).unwrap();
        assert_eq!(f, back);
    }

    /// 拼音查出来的候选，`code` 与查询串**不是一回事**（§6.3 的数据前提）。
    /// 这条把「码跟着候选走」钉死：用 `hao` 查「好」，码必须是 `vb`。
    #[test]
    fn entry_code_is_not_the_query_string() {
        let f = Frame::Result {
            code: "hao".into(),
            source: source::PINYIN,
            primary_len: 2,
            entries: vec![
                Entry {
                    word: "好".into(),
                    label: "全码".into(),
                    code: "vb".into(),
                    hint_at: 0,
                    kind: EntryKind::Word,
                },
                Entry {
                    word: "号".into(),
                    label: "全码".into(),
                    code: "kgn".into(),
                    hint_at: 0,
                    kind: EntryKind::Word,
                },
            ],
        };
        let back = Frame::decode(&f.encode()).unwrap();
        match back {
            Frame::Result { code, entries, .. } => {
                assert_eq!(code, "hao", "查询串与码必须各自独立");
                assert_eq!(entries[0].code, "vb");
                assert_eq!(entries[1].code, "kgn");
            }
            other => panic!("解出了别的帧：{other:?}"),
        }
    }

    /// 空 `code` = 本次不提示，必须能原样往返（不提示是最常见的情况：
    /// 五笔段、`hint.mode = off`、以及所有不做提示的场景）。
    #[test]
    fn empty_code_roundtrips() {
        let f = Frame::Result {
            code: "vbg".into(),
            source: source::WUBI,
            primary_len: 1,
            entries: vec![Entry {
                word: "好".into(),
                label: "全码".into(),
                code: String::new(),
                hint_at: 0,
                kind: EntryKind::Word,
            }],
        };
        assert_eq!(Frame::decode(&f.encode()).unwrap(), f);
    }

    /// v6 的词中位置（D64）：`hint_at` 必须原样往返。多段组合
    /// `hao'ktn` → 好吃 的 chip 画在「好」后面（`hint_at = 1`），
    /// 挂词尾会被误读成整个词的五笔码。
    #[test]
    fn mid_word_hint_position_roundtrips() {
        let f = Frame::Result {
            code: "hao'ktn".into(),
            source: source::PINYIN,
            primary_len: 1,
            entries: vec![Entry {
                word: "好吃".into(),
                label: "多段".into(),
                code: "vbg".into(),
                hint_at: 1,
                kind: EntryKind::Word,
            }],
        };
        let back = Frame::decode(&f.encode()).unwrap();
        assert_eq!(f, back);
        match back {
            Frame::Result { entries, .. } => {
                assert_eq!(entries[0].hint_at, 1, "chip 挂在第 1 个字符之后");
            }
            other => panic!("解出了别的帧：{other:?}"),
        }
    }

    /// v8 的候选种类：emoji 候选的 `word` 是多字节 UTF-8，而且常常紧跟在一个
    /// 带 chip 的词后面——三个追加字段（`code` / `hint_at` / `kind`）的**先后
    /// 顺序**必须与解码一致，否则 chip 的词中位置会被读成 emoji 的第一个字节。
    #[test]
    fn entry_kind_roundtrips() {
        let f = Frame::Result {
            code: "vbyg".into(),
            source: source::WUBI,
            primary_len: 1,
            entries: vec![
                Entry {
                    word: "好评".into(),
                    label: "全码".into(),
                    code: "vbyg".into(),
                    hint_at: 2,
                    kind: EntryKind::Word,
                },
                Entry {
                    word: "👍".into(),
                    label: String::new(),
                    code: String::new(),
                    hint_at: 0,
                    kind: EntryKind::Emoji,
                },
            ],
        };
        let back = Frame::decode(&f.encode()).unwrap();
        assert_eq!(f, back);
        match back {
            Frame::Result { entries, .. } => {
                assert_eq!(
                    entries[0].hint_at, 2,
                    "chip 位置不能被 emoji 的种类字节顶掉"
                );
                assert_eq!(entries[0].kind, EntryKind::Word);
                assert_eq!(entries[1].word, "👍");
                assert_eq!(entries[1].kind, EntryKind::Emoji);
            }
            other => panic!("解出了别的帧：{other:?}"),
        }
    }

    /// 不认识的种类字节必须报错，不能猜成词——猜错的话 emoji 会被当词上报学习。
    #[test]
    fn unknown_entry_kind_is_rejected() {
        let f = Frame::Result {
            code: "vbyg".into(),
            source: source::WUBI,
            primary_len: 1,
            entries: vec![Entry {
                word: "好评".into(),
                label: "全码".into(),
                code: "vbyg".into(),
                hint_at: 0,
                kind: EntryKind::Word,
            }],
        };
        let mut bytes = f.encode();
        // 帧尾就是那条候选的 kind 字节：把它改成 9 再解。
        let last = bytes.len() - 1;
        bytes[last] = 9;
        assert_eq!(Frame::decode(&bytes), Err(DecodeError::BadEntryKind(9)));
    }
}
