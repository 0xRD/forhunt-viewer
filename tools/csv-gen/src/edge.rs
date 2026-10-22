//! Edge-case suite: small files exercising every malformed or unusual shape
//! the viewer must survive. Fully deterministic; the engine's golden tests
//! open each of them.

use crate::rng::Rng;

pub struct Case {
    pub name: &'static str,
    pub bytes: Vec<u8>,
    pub what: &'static str,
}

fn c(name: &'static str, what: &'static str, bytes: impl Into<Vec<u8>>) -> Case {
    Case {
        name,
        what,
        bytes: bytes.into(),
    }
}

fn utf16(s: &str, be: bool) -> Vec<u8> {
    let mut v = if be {
        vec![0xFE, 0xFF]
    } else {
        vec![0xFF, 0xFE]
    };
    for u in s.encode_utf16() {
        let b = if be { u.to_be_bytes() } else { u.to_le_bytes() };
        v.extend_from_slice(&b);
    }
    v
}

const SIMPLE: &str = "Time,EventId,User,Message\n\
2024-03-11 10:00:00.0000000,4624,jdoe,Logon\n\
2024-03-11 10:00:01.0000000,4625,asmith,Failed logon\n\
2024-03-11 10:00:02.0000000,4688,jdoe,\"cmd.exe /c \"\"whoami\"\"\"\n";

pub fn cases() -> Vec<Case> {
    let mut v = vec![
        c("empty.csv", "zero bytes", ""),
        c(
            "header_only.csv",
            "header, no rows, no final newline",
            "a,b,c",
        ),
        c("header_only_nl.csv", "header and newline only", "a,b,c\n"),
        c("simple_lf.csv", "baseline, LF, escaped quotes", SIMPLE),
        c(
            "simple_crlf.csv",
            "baseline with CRLF",
            SIMPLE.replace('\n', "\r\n"),
        ),
        c(
            "mixed_eol.csv",
            "LF and CRLF line endings mixed",
            "a,b\r\n1,2\n3,4\r\n5,6\n",
        ),
        c(
            "bom_utf8.csv",
            "UTF-8 BOM",
            [b"\xEF\xBB\xBF".as_slice(), SIMPLE.as_bytes()].concat(),
        ),
        c(
            "utf16le.csv",
            "UTF-16LE with BOM, non-ASCII",
            utf16("Name,City\nJosé,São Paulo\n李,北京\n", false),
        ),
        c(
            "utf16be.csv",
            "UTF-16BE with BOM",
            utf16("Name,City\nJosé,São Paulo\n李,北京\n", true),
        ),
        c(
            "semicolon.csv",
            "semicolon delimiter, decimal commas",
            "id;amount;label\n1;3,14;a\n2;2,72;\"b;c\"\n3;1,41;d\n",
        ),
        c(
            "tab.tsv",
            "tab delimiter",
            "id\tname\tnote\n1\talpha\tx y\n2\tbeta\t\"q\tq\"\n",
        ),
        c(
            "pipe.csv",
            "pipe delimiter",
            "id|name|note\n1|alpha|x\n2|beta|y\n3|gamma|z\n",
        ),
        c(
            "embedded_newlines.csv",
            "quoted fields with LF and CRLF inside",
            "id,text,n\n1,\"line one\nline two\",a\n2,\"crlf\r\ninside\",b\n3,\"\n\",c\n4,plain,d\n",
        ),
        c(
            "ragged.csv",
            "rows with too few and too many fields",
            "a,b,c\n1,2,3\n4,5\n6,7,8,9\n10,11,12\n\n13,14,15\n",
        ),
        c(
            "stray_quotes.csv",
            "quotes inside unquoted fields are literal",
            "a,b,c\n1,he said \"hi\",3\n4,5\" tall,6\n7,8,9\n",
        ),
        c(
            "quote_after_close.csv",
            "text after a closing quote",
            "a,b\n\"x\"y,2\n3,4\n",
        ),
        c(
            "unterminated_quote.csv",
            "a quote opened and never closed (runs to EOF)",
            "a,b\n1,2\n3,\"never closed\n4,5\n6,7\n",
        ),
        c(
            "truncated.csv",
            "last row cut mid-field, no final newline",
            "a,b,c\n1,2,3\n4,5,6\n7,8",
        ),
        c(
            "truncated_in_quote.csv",
            "file ends inside a quoted field",
            "a,b\n1,2\n3,\"cut her",
        ),
        c(
            "nul_bytes.csv",
            "NUL bytes inside cells",
            b"a,b\n1,x\x00y\n2,\x00\x00\n3,z\n".to_vec(),
        ),
        c(
            "invalid_utf8.csv",
            "invalid UTF-8 sequences",
            b"a,b\n1,caf\xE9\n2,\xFF\xFE\xFD\n3,ok\n".to_vec(),
        ),
        c(
            "latin1.csv",
            "Windows-1252 text without BOM",
            b"name,city\nJos\xE9,Montr\xE9al\nM\xFCller,K\xF6ln\n".to_vec(),
        ),
        c(
            "blank_lines.csv",
            "blank lines between rows and at the end",
            "a,b\n\n1,2\n\n\n3,4\n\n",
        ),
        c(
            "duplicate_headers.csv",
            "duplicate and empty header names",
            "a,a,,b\n1,2,3,4\n",
        ),
        c(
            "single_column.csv",
            "one column, no delimiter anywhere",
            "value\nalpha\nbeta\ngamma\n",
        ),
        c(
            "no_header.csv",
            "numeric first row: no header",
            "1,2024-03-11 10:00:00,x\n2,2024-03-11 10:00:01,y\n3,2024-03-11 10:00:02,z\n",
        ),
        c(
            "quoted_header.csv",
            "quoted header names with delimiters",
            "\"Event, Id\",\"Time\"\n1,2024-01-01\n",
        ),
        c(
            "only_quotes.csv",
            "cells that are just quotes",
            "a,b\n\"\",\"\"\"\"\n\"\"\"\"\"\",x\n",
        ),
        c(
            "spaces_around.csv",
            "spaces around delimiters and quotes",
            "a , b\n 1 , \"2\" \n",
        ),
        c(
            "times_mixed.csv",
            "time formats in one column",
            "t,v\n2024-03-11T10:00:00Z,1\n2024-03-11 10:00:01.1234567,2\n2024-03-11T12:00:02+02:00,3\n03/11/2024 10:00:03,4\n,5\nnot a date,6\n",
        ),
    ];

    // Wide: 1000 columns.
    let mut wide = String::new();
    for i in 0..1000 {
        if i > 0 {
            wide.push(',');
        }
        wide.push_str(&format!("c{i}"));
    }
    wide.push('\n');
    for r in 0..3 {
        for i in 0..1000 {
            if i > 0 {
                wide.push(',');
            }
            wide.push_str(&(r * 1000 + i).to_string());
        }
        wide.push('\n');
    }
    v.push(c("wide_1000_cols.csv", "1000 columns", wide));

    // 1 MiB cells, one plain and one quoted with newlines inside.
    let mut rng = Rng::new(42);
    let mut big = String::from("id,blob,tail\n1,");
    for _ in 0..(1 << 20) {
        big.push((b'a' + rng.below(26) as u8) as char);
    }
    big.push_str(",end\n2,\"");
    for k in 0..(1 << 20) {
        big.push(if k % 1000 == 999 { '\n' } else { 'x' });
    }
    big.push_str("\",end\n3,small,end\n");
    v.push(c(
        "big_cells.csv",
        "1 MiB unquoted cell and 1 MiB quoted multi-line cell",
        big,
    ));

    // Many short rows (row index blocks > 1).
    let mut many = String::from("n,sq\n");
    for i in 0..5000u64 {
        many.push_str(&format!("{i},{}\n", i * i));
    }
    v.push(c(
        "many_rows.csv",
        "5000 rows (spans several index blocks)",
        many,
    ));
    v
}
