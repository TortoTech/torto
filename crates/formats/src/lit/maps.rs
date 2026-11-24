// SPDX-License-Identifier: GPL-3.0-only
// LIT protocol mappings recorded by ConvertLIT and calibre.
// Copyright (c) 2008 Marshall T. Vandegrift (calibre).
// Rust table port from eruditio by Ryker Zhu (Apache-2.0).
//! LIT binary-to-HTML tag and attribute decode tables.
//!
//! Ported from calibre's `ebooks/lit/maps/html.py` and `maps/opf.py`.
//! These tables map integer codes in the LIT binary format to HTML/OPF
//! tag names and attribute names.

// ---------------------------------------------------------------------------
// Lookup helper
// ---------------------------------------------------------------------------

/// Binary-search a sorted `(u16, &str)` slice for a given code.
fn lookup<'a>(table: &'a [(u16, &'a str)], code: u16) -> Option<&'a str> {
    table
        .binary_search_by_key(&code, |&(k, _)| k)
        .ok()
        .map(|i| table[i].1)
}

// ---------------------------------------------------------------------------
// HTML tag table (109 entries, indexed 0..=108)
// ---------------------------------------------------------------------------

pub static HTML_TAGS: [Option<&str>; 109] = [
    None,               // 0
    None,               // 1
    None,               // 2
    Some("a"),          // 3
    Some("acronym"),    // 4
    Some("address"),    // 5
    Some("applet"),     // 6
    Some("area"),       // 7
    Some("b"),          // 8
    Some("base"),       // 9
    Some("basefont"),   // 10
    Some("bdo"),        // 11
    Some("bgsound"),    // 12
    Some("big"),        // 13
    Some("blink"),      // 14
    Some("blockquote"), // 15
    Some("body"),       // 16
    Some("br"),         // 17
    Some("button"),     // 18
    Some("caption"),    // 19
    Some("center"),     // 20
    Some("cite"),       // 21
    Some("code"),       // 22
    Some("col"),        // 23
    Some("colgroup"),   // 24
    None,               // 25
    None,               // 26
    Some("dd"),         // 27
    Some("del"),        // 28
    Some("dfn"),        // 29
    Some("dir"),        // 30
    Some("div"),        // 31
    Some("dl"),         // 32
    Some("dt"),         // 33
    Some("em"),         // 34
    Some("embed"),      // 35
    Some("fieldset"),   // 36
    Some("font"),       // 37
    Some("form"),       // 38
    Some("frame"),      // 39
    Some("frameset"),   // 40
    None,               // 41
    Some("h1"),         // 42
    Some("h2"),         // 43
    Some("h3"),         // 44
    Some("h4"),         // 45
    Some("h5"),         // 46
    Some("h6"),         // 47
    Some("head"),       // 48
    Some("hr"),         // 49
    Some("html"),       // 50
    Some("i"),          // 51
    Some("iframe"),     // 52
    Some("img"),        // 53
    Some("input"),      // 54
    Some("ins"),        // 55
    Some("kbd"),        // 56
    Some("label"),      // 57
    Some("legend"),     // 58
    Some("li"),         // 59
    Some("link"),       // 60
    Some("tag61"),      // 61
    Some("map"),        // 62
    Some("tag63"),      // 63
    Some("tag64"),      // 64
    Some("meta"),       // 65
    Some("nextid"),     // 66
    Some("nobr"),       // 67
    Some("noembed"),    // 68
    Some("noframes"),   // 69
    Some("noscript"),   // 70
    Some("object"),     // 71
    Some("ol"),         // 72
    Some("option"),     // 73
    Some("p"),          // 74
    Some("param"),      // 75
    Some("plaintext"),  // 76
    Some("pre"),        // 77
    Some("q"),          // 78
    Some("rp"),         // 79
    Some("rt"),         // 80
    Some("ruby"),       // 81
    Some("s"),          // 82
    Some("samp"),       // 83
    Some("script"),     // 84
    Some("select"),     // 85
    Some("small"),      // 86
    Some("span"),       // 87
    Some("strike"),     // 88
    Some("strong"),     // 89
    Some("style"),      // 90
    Some("sub"),        // 91
    Some("sup"),        // 92
    Some("table"),      // 93
    Some("tbody"),      // 94
    Some("tc"),         // 95
    Some("td"),         // 96
    Some("textarea"),   // 97
    Some("tfoot"),      // 98
    Some("th"),         // 99
    Some("thead"),      // 100
    Some("title"),      // 101
    Some("tr"),         // 102
    Some("tt"),         // 103
    Some("u"),          // 104
    Some("ul"),         // 105
    Some("var"),        // 106
    Some("wbr"),        // 107
    None,               // 108
];

// ---------------------------------------------------------------------------
// HTML global attributes (ATTRS0) — sorted by code for binary search
// ---------------------------------------------------------------------------

pub(super) static HTML_GLOBAL_ATTRS: &[(u16, &str)] = &[
    (0x8010, "tabindex"),
    (0x8046, "title"),
    (0x804b, "style"),
    (0x804d, "disabled"),
    (0x83ea, "class"),
    (0x83eb, "id"),
    (0x83fe, "datafld"),
    (0x83ff, "datasrc"),
    (0x8400, "dataformatas"),
    (0x87d6, "accesskey"),
    (0x9392, "lang"),
    (0x93ed, "language"),
    (0x93fe, "dir"),
    (0x9771, "onmouseover"),
    (0x9772, "onmouseout"),
    (0x9773, "onmousedown"),
    (0x9774, "onmouseup"),
    (0x9775, "onmousemove"),
    (0x9776, "onkeydown"),
    (0x9777, "onkeyup"),
    (0x9778, "onkeypress"),
    (0x9779, "onclick"),
    (0x977a, "ondblclick"),
    (0x977e, "onhelp"),
    (0x977f, "onfocus"),
    (0x9780, "onblur"),
    (0x9783, "onrowexit"),
    (0x9784, "onrowenter"),
    (0x9786, "onbeforeupdate"),
    (0x9787, "onafterupdate"),
    (0x978a, "onreadystatechange"),
    (0x9790, "onscroll"),
    (0x9794, "ondragstart"),
    (0x9795, "onresize"),
    (0x9796, "onselectstart"),
    (0x9797, "onerrorupdate"),
    (0x9799, "ondatasetchanged"),
    (0x979a, "ondataavailable"),
    (0x979b, "ondatasetcomplete"),
    (0x979c, "onfilterchange"),
    (0x979f, "onlosecapture"),
    (0x97a0, "onpropertychange"),
    (0x97a2, "ondrag"),
    (0x97a3, "ondragend"),
    (0x97a4, "ondragenter"),
    (0x97a5, "ondragover"),
    (0x97a6, "ondragleave"),
    (0x97a7, "ondrop"),
    (0x97a8, "oncut"),
    (0x97a9, "oncopy"),
    (0x97aa, "onpaste"),
    (0x97ab, "onbeforecut"),
    (0x97ac, "onbeforecopy"),
    (0x97ad, "onbeforepaste"),
    (0x97af, "onrowsdelete"),
    (0x97b0, "onrowsinserted"),
    (0x97b1, "oncellchange"),
    (0x97b2, "oncontextmenu"),
    (0x97b6, "onbeforeeditfocus"),
];

// ---------------------------------------------------------------------------
// Per-tag HTML attributes — sorted by code for binary search
// ---------------------------------------------------------------------------

// <a> (3)
pub(super) static ATTRS_A: &[(u16, &str)] = &[
    (0x0001, "href"),
    (0x03ec, "target"),
    (0x03ee, "rel"),
    (0x03ef, "rev"),
    (0x03f0, "urn"),
    (0x03f1, "methods"),
    (0x8001, "name"),
    (0x8046, "title"),
    (0x804b, "style"),
    (0x83ea, "class"),
    (0x83eb, "id"),
];
// <address> (5)
pub(super) static ATTRS_ADDRESS: &[(u16, &str)] = &[(0x9399, "clear")];
// <applet> (6)
pub(super) static ATTRS_APPLET: &[(u16, &str)] = &[
    (0x8001, "name"),
    (0x8006, "width"),
    (0x8007, "height"),
    (0x804a, "align"),
    (0x8bbb, "classid"),
    (0x8bbc, "data"),
    (0x8bbf, "codebase"),
    (0x8bc0, "codetype"),
    (0x8bc1, "code"),
    (0x8bc2, "type"),
    (0x8bc5, "vspace"),
    (0x8bc6, "hspace"),
    (0x978e, "onerror"),
];
// <area> (7)
pub(super) static ATTRS_AREA: &[(u16, &str)] = &[
    (0x0001, "href"),
    (0x03ea, "shape"),
    (0x03eb, "coords"),
    (0x03ed, "target"),
    (0x03ee, "alt"),
    (0x03ef, "nohref"),
    (0x8046, "title"),
    (0x804b, "style"),
    (0x83ea, "class"),
    (0x83eb, "id"),
];
// <b> (8)
pub(super) static ATTRS_STYLE_CLASS_ID: &[(u16, &str)] = &[
    (0x8046, "title"),
    (0x804b, "style"),
    (0x83ea, "class"),
    (0x83eb, "id"),
];
// <base> (9)
pub(super) static ATTRS_BASE: &[(u16, &str)] = &[(0x03ec, "href"), (0x03ed, "target")];
// <basefont> (10)
pub(super) static ATTRS_BASEFONT: &[(u16, &str)] =
    &[(0x938b, "color"), (0x939b, "face"), (0x93a3, "size")];
// <bgsound> (12)
pub(super) static ATTRS_BGSOUND: &[(u16, &str)] = &[
    (0x03ea, "src"),
    (0x03eb, "loop"),
    (0x03ec, "volume"),
    (0x03ed, "balance"),
];
// <blockquote> (15)
pub(super) static ATTRS_CLEAR_STYLE: &[(u16, &str)] = &[
    (0x8046, "title"),
    (0x804b, "style"),
    (0x83ea, "class"),
    (0x83eb, "id"),
    (0x9399, "clear"),
];
// <body> (16)
pub(super) static ATTRS_BODY: &[(u16, &str)] = &[
    (0x07db, "link"),
    (0x07dc, "alink"),
    (0x07dd, "vlink"),
    (0x8046, "title"),
    (0x804b, "style"),
    (0x83ea, "class"),
    (0x83eb, "id"),
    (0x938a, "background"),
    (0x938b, "text"),
    (0x938e, "nowrap"),
    (0x93ae, "topmargin"),
    (0x93af, "rightmargin"),
    (0x93b0, "bottommargin"),
    (0x93b1, "leftmargin"),
    (0x93b6, "bgproperties"),
    (0x93d8, "scroll"),
    (0x977b, "onselect"),
    (0x9791, "onload"),
    (0x9792, "onunload"),
    (0x9798, "onbeforeunload"),
    (0x97b3, "onbeforeprint"),
    (0x97b4, "onafterprint"),
    (0xfe0c, "bgcolor"),
];
// <button> (18)
pub(super) static ATTRS_BUTTON: &[(u16, &str)] = &[(0x07d1, "type"), (0x8001, "name")];
// <caption> (19)
pub(super) static ATTRS_CAPTION: &[(u16, &str)] = &[
    (0x8046, "title"),
    (0x8049, "align"),
    (0x804b, "style"),
    (0x83ea, "class"),
    (0x83eb, "id"),
    (0x93a8, "valign"),
];
// <center> (20), <cite> (21), <code> (22), <dfn> (29)
// reuse ATTRS_CLEAR_STYLE or ATTRS_STYLE_CLASS_ID as appropriate

// <col> (23), <colgroup> (24)
pub(super) static ATTRS_COL: &[(u16, &str)] = &[
    (0x03ea, "span"),
    (0x8006, "width"),
    (0x8049, "align"),
    (0x93a8, "valign"),
    (0xfe0c, "bgcolor"),
];
// <dd> (27)
pub(super) static ATTRS_DD: &[(u16, &str)] = &[
    (0x8046, "title"),
    (0x804b, "style"),
    (0x83ea, "class"),
    (0x83eb, "id"),
    (0x938e, "nowrap"),
];
// <div> (31)
pub(super) static ATTRS_DIV: &[(u16, &str)] = &[
    (0x8046, "title"),
    (0x8049, "align"),
    (0x804b, "style"),
    (0x83ea, "class"),
    (0x83eb, "id"),
    (0x938e, "nowrap"),
];
// <dl> (32)
pub(super) static ATTRS_DL: &[(u16, &str)] = &[
    (0x03ea, "compact"),
    (0x8046, "title"),
    (0x804b, "style"),
    (0x83ea, "class"),
    (0x83eb, "id"),
];
// <embed> (35)
pub(super) static ATTRS_EMBED: &[(u16, &str)] = &[
    (0x8001, "name"),
    (0x8006, "width"),
    (0x8007, "height"),
    (0x804a, "align"),
    (0x8bbd, "palette"),
    (0x8bbe, "pluginspage"),
    (0x8bbf, "src"),
    (0x8bc1, "units"),
    (0x8bc2, "type"),
    (0x8bc3, "hidden"),
];
// <fieldset> (36)
pub(super) static ATTRS_FIELDSET: &[(u16, &str)] = &[(0x804a, "align")];
// <font> (37)
pub(super) static ATTRS_FONT: &[(u16, &str)] = &[
    (0x8046, "title"),
    (0x804b, "style"),
    (0x83ea, "class"),
    (0x83eb, "id"),
    (0x938b, "color"),
    (0x939b, "face"),
    (0x939c, "size"),
];
// <form> (38)
pub(super) static ATTRS_FORM: &[(u16, &str)] = &[
    (0x03ea, "action"),
    (0x03ec, "enctype"),
    (0x03ed, "method"),
    (0x03ef, "target"),
    (0x03f4, "accept-charset"),
    (0x8001, "name"),
    (0x977c, "onsubmit"),
    (0x977d, "onreset"),
];
// <frame> (39)
pub(super) static ATTRS_FRAME: &[(u16, &str)] = &[
    (0x8000, "align"),
    (0x8001, "name"),
    (0x8bb9, "src"),
    (0x8bbb, "border"),
    (0x8bbc, "frameborder"),
    (0x8bbd, "framespacing"),
    (0x8bbe, "marginwidth"),
    (0x8bbf, "marginheight"),
    (0x8bc0, "noresize"),
    (0x8bc1, "scrolling"),
    (0x8fa2, "bordercolor"),
];
// <frameset> (40)
pub(super) static ATTRS_FRAMESET: &[(u16, &str)] = &[
    (0x03e9, "rows"),
    (0x03ea, "cols"),
    (0x03eb, "border"),
    (0x03ec, "bordercolor"),
    (0x03ed, "frameborder"),
    (0x03ee, "framespacing"),
    (0x8001, "name"),
    (0x9791, "onload"),
    (0x9792, "onunload"),
    (0x9798, "onbeforeunload"),
    (0x97b3, "onbeforeprint"),
    (0x97b4, "onafterprint"),
];
// <h1..h6> (42-47): align + clear + style/class/id
pub(super) static ATTRS_HEADING: &[(u16, &str)] = &[
    (0x8046, "title"),
    (0x8049, "align"),
    (0x804b, "style"),
    (0x83ea, "class"),
    (0x83eb, "id"),
    (0x9399, "clear"),
];
// <hr> (49)
pub(super) static ATTRS_HR: &[(u16, &str)] = &[
    (0x03ea, "noshade"),
    (0x8006, "width"),
    (0x8007, "size"),
    (0x8046, "title"),
    (0x8049, "align"),
    (0x804b, "style"),
    (0x83ea, "class"),
    (0x83eb, "id"),
    (0x938b, "color"),
];
// <iframe> (52)
pub(super) static ATTRS_IFRAME: &[(u16, &str)] = &[
    (0x8001, "name"),
    (0x8006, "width"),
    (0x8007, "height"),
    (0x804a, "align"),
    (0x8bb9, "src"),
    (0x8bbb, "border"),
    (0x8bbc, "frameborder"),
    (0x8bbd, "framespacing"),
    (0x8bbe, "marginwidth"),
    (0x8bbf, "marginheight"),
    (0x8bc0, "noresize"),
    (0x8bc1, "scrolling"),
    (0x8fa2, "vspace"),
    (0x8fa3, "hspace"),
];
// <img> (53)
pub(super) static ATTRS_IMG: &[(u16, &str)] = &[
    (0x03eb, "alt"),
    (0x03ec, "src"),
    (0x03ed, "border"),
    (0x03ee, "vspace"),
    (0x03ef, "hspace"),
    (0x03f0, "lowsrc"),
    (0x03f1, "vrml"),
    (0x03f2, "dynsrc"),
    (0x03f4, "loop"),
    (0x03f6, "start"),
    (0x07d3, "ismap"),
    (0x07d9, "usemap"),
    (0x8001, "name"),
    (0x8006, "width"),
    (0x8007, "height"),
    (0x8046, "title"),
    (0x804a, "align"),
    (0x804b, "style"),
    (0x83ea, "class"),
    (0x83eb, "id"),
    (0x978d, "onabort"),
    (0x978e, "onerror"),
    (0x9791, "onload"),
];
// <input> (54)
pub(super) static ATTRS_INPUT: &[(u16, &str)] = &[
    (0x07d1, "type"),
    (0x07d3, "size"),
    (0x07d4, "maxlength"),
    (0x07d6, "readonly"),
    (0x07d8, "indeterminate"),
    (0x07da, "checked"),
    (0x07db, "alt"),
    (0x07dc, "src"),
    (0x07dd, "border"),
    (0x07de, "vspace"),
    (0x07df, "hspace"),
    (0x07e0, "lowsrc"),
    (0x07e1, "vrml"),
    (0x07e2, "dynsrc"),
    (0x07e4, "loop"),
    (0x07e5, "start"),
    (0x8001, "name"),
    (0x8006, "width"),
    (0x8007, "height"),
    (0x804a, "align"),
    (0x93ee, "value"),
    (0x977b, "onselect"),
    (0x978d, "onabort"),
    (0x978e, "onerror"),
    (0x978f, "onchange"),
    (0x9791, "onload"),
];
// <label> (57)
pub(super) static ATTRS_LABEL: &[(u16, &str)] = &[(0x03e9, "for")];
// <legend> (58)
pub(super) static ATTRS_LEGEND: &[(u16, &str)] = &[(0x804a, "align")];
// <li> (59)
pub(super) static ATTRS_LI: &[(u16, &str)] = &[
    (0x03ea, "value"),
    (0x8046, "title"),
    (0x804b, "style"),
    (0x83ea, "class"),
    (0x83eb, "id"),
    (0x939a, "type"),
];
// <link> (60)
pub(super) static ATTRS_LINK: &[(u16, &str)] = &[
    (0x03ee, "href"),
    (0x03ef, "rel"),
    (0x03f0, "rev"),
    (0x03f1, "type"),
    (0x03f9, "media"),
    (0x03fa, "target"),
    (0x8046, "title"),
    (0x804b, "style"),
    (0x83ea, "class"),
    (0x83eb, "id"),
    (0x978e, "onerror"),
    (0x9791, "onload"),
];
// <tag61> (61)
pub(super) static ATTRS_TAG61: &[(u16, &str)] = &[(0x9399, "clear")];
// <map> (62)
pub(super) static ATTRS_MAP: &[(u16, &str)] = &[
    (0x8001, "name"),
    (0x8046, "title"),
    (0x804b, "style"),
    (0x83ea, "class"),
    (0x83eb, "id"),
];
// <tag63> (63) — marquee
pub(super) static ATTRS_TAG63: &[(u16, &str)] = &[
    (0x1771, "scrolldelay"),
    (0x1772, "direction"),
    (0x1773, "behavior"),
    (0x1774, "scrollamount"),
    (0x1775, "loop"),
    (0x1776, "vspace"),
    (0x1777, "hspace"),
    (0x1778, "truespeed"),
    (0x8006, "width"),
    (0x8007, "height"),
    (0x9785, "onbounce"),
    (0x978b, "onfinish"),
    (0x978c, "onstart"),
    (0xfe0c, "bgcolor"),
];
// <meta> (65)
pub(super) static ATTRS_META: &[(u16, &str)] = &[
    (0x03ea, "http-equiv"),
    (0x03eb, "content"),
    (0x03ec, "url"),
    (0x03f6, "charset"),
    (0x8001, "name"),
];
// <nextid> (66)
pub(super) static ATTRS_NEXTID: &[(u16, &str)] = &[(0x03f5, "n")];
// <object> (71)
pub(super) static ATTRS_OBJECT: &[(u16, &str)] = &[
    (0x8000, "usemap"),
    (0x8001, "name"),
    (0x8006, "width"),
    (0x8007, "height"),
    (0x8046, "title"),
    (0x804a, "align"),
    (0x804b, "style"),
    (0x83ea, "class"),
    (0x83eb, "id"),
    (0x8bbb, "classid"),
    (0x8bbc, "data"),
    (0x8bbf, "codebase"),
    (0x8bc0, "codetype"),
    (0x8bc1, "code"),
    (0x8bc2, "type"),
    (0x8bc5, "vspace"),
    (0x8bc6, "hspace"),
    (0x978e, "onerror"),
];
// <ol> (72)
pub(super) static ATTRS_OL: &[(u16, &str)] = &[
    (0x03eb, "compact"),
    (0x03ec, "start"),
    (0x8046, "title"),
    (0x804b, "style"),
    (0x83ea, "class"),
    (0x83eb, "id"),
    (0x939a, "type"),
];
// <option> (73)
pub(super) static ATTRS_OPTION: &[(u16, &str)] = &[(0x03ea, "selected"), (0x03eb, "value")];
// <p> (74)
pub(super) static ATTRS_P: &[(u16, &str)] = &[
    (0x8046, "title"),
    (0x8049, "align"),
    (0x804b, "style"),
    (0x83ea, "class"),
    (0x83eb, "id"),
    (0x9399, "clear"),
];
// <param> (75)
pub(super) static ATTRS_PARAM: &[(u16, &str)] = &[(0x8000, "type")];
// <plaintext> (76)
pub(super) static ATTRS_PLAINTEXT: &[(u16, &str)] = &[(0x9399, "clear")];
// <script> (84)
pub(super) static ATTRS_SCRIPT: &[(u16, &str)] = &[
    (0x03ea, "src"),
    (0x03ed, "for"),
    (0x03ee, "event"),
    (0x03f0, "defer"),
    (0x03f2, "type"),
    (0x978e, "onerror"),
];
// <select> (85)
pub(super) static ATTRS_SELECT: &[(u16, &str)] = &[
    (0x03eb, "size"),
    (0x03ec, "multiple"),
    (0x8000, "align"),
    (0x8001, "name"),
    (0x978f, "onchange"),
];
// <style> (90)
pub(super) static ATTRS_STYLE: &[(u16, &str)] = &[
    (0x03eb, "type"),
    (0x03ef, "media"),
    (0x8046, "title"),
    (0x978e, "onerror"),
    (0x9791, "onload"),
];
// <table> (93)
pub(super) static ATTRS_TABLE: &[(u16, &str)] = &[
    (0x03ea, "cols"),
    (0x03eb, "border"),
    (0x03ec, "rules"),
    (0x03ed, "frame"),
    (0x03ee, "cellspacing"),
    (0x03ef, "cellpadding"),
    (0x03fa, "datapagesize"),
    (0x8006, "width"),
    (0x8007, "height"),
    (0x8046, "title"),
    (0x804a, "align"),
    (0x804b, "style"),
    (0x83ea, "class"),
    (0x83eb, "id"),
    (0x938a, "background"),
    (0x93a5, "bordercolor"),
    (0x93a6, "bordercolorlight"),
    (0x93a7, "bordercolordark"),
    (0xfe0c, "bgcolor"),
];
// <tbody> (94), <tfoot> (98), <thead> (100)
pub(super) static ATTRS_TBODY: &[(u16, &str)] =
    &[(0x8049, "align"), (0x93a8, "valign"), (0xfe0c, "bgcolor")];
// <tc> (95) — just align/valign
pub(super) static ATTRS_TC: &[(u16, &str)] = &[(0x8049, "align"), (0x93a8, "valign")];
// <td> (96), <th> (99)
pub(super) static ATTRS_TD: &[(u16, &str)] = &[
    (0x07d2, "rowspan"),
    (0x07d3, "colspan"),
    (0x8006, "width"),
    (0x8007, "height"),
    (0x8046, "title"),
    (0x8049, "align"),
    (0x804b, "style"),
    (0x83ea, "class"),
    (0x83eb, "id"),
    (0x938a, "background"),
    (0x938e, "nowrap"),
    (0x93a5, "bordercolor"),
    (0x93a6, "bordercolorlight"),
    (0x93a7, "bordercolordark"),
    (0x93a8, "valign"),
    (0xfe0c, "bgcolor"),
];
// <textarea> (97)
pub(super) static ATTRS_TEXTAREA: &[(u16, &str)] = &[
    (0x1b5a, "rows"),
    (0x1b5b, "cols"),
    (0x1b5c, "wrap"),
    (0x1b5d, "readonly"),
    (0x8001, "name"),
    (0x977b, "onselect"),
    (0x978f, "onchange"),
];
// <tr> (102)
pub(super) static ATTRS_TR: &[(u16, &str)] = &[
    (0x8007, "height"),
    (0x8046, "title"),
    (0x8049, "align"),
    (0x804b, "style"),
    (0x83ea, "class"),
    (0x83eb, "id"),
    (0x93a5, "bordercolor"),
    (0x93a6, "bordercolorlight"),
    (0x93a7, "bordercolordark"),
    (0x93a8, "valign"),
    (0xfe0c, "bgcolor"),
];
// <ul> (105)
pub(super) static ATTRS_UL: &[(u16, &str)] = &[
    (0x03eb, "compact"),
    (0x8046, "title"),
    (0x804b, "style"),
    (0x83ea, "class"),
    (0x83eb, "id"),
    (0x939a, "type"),
];
// <wbr> (107) — just clear
pub(super) static ATTRS_WBR: &[(u16, &str)] = &[(0x9399, "clear")];

// ---------------------------------------------------------------------------
// Per-tag attribute dispatch
// ---------------------------------------------------------------------------

/// Look up a per-tag HTML attribute by tag index and attribute code.
pub(crate) fn html_tag_attr(tag_index: usize, code: u16) -> Option<&'static str> {
    let table: &[(u16, &str)] = match tag_index {
        3 => ATTRS_A,
        5 => ATTRS_ADDRESS,
        6 => ATTRS_APPLET,
        7 => ATTRS_AREA,
        8 | 13 | 29 | 34 | 51 | 56 | 78 | 82 | 83 | 86 | 87 | 88 | 89 | 91 | 92 | 103 | 104
        | 106 => ATTRS_STYLE_CLASS_ID,
        9 => ATTRS_BASE,
        10 => ATTRS_BASEFONT,
        12 => ATTRS_BGSOUND,
        15 | 17 | 20 | 77 => ATTRS_CLEAR_STYLE,
        16 => ATTRS_BODY,
        18 => ATTRS_BUTTON,
        19 => ATTRS_CAPTION,
        21 | 22 => ATTRS_STYLE_CLASS_ID,
        23 | 24 => ATTRS_COL,
        27 | 33 => ATTRS_DD,
        31 => ATTRS_DIV,
        32 => ATTRS_DL,
        35 => ATTRS_EMBED,
        36 => ATTRS_FIELDSET,
        37 => ATTRS_FONT,
        38 => ATTRS_FORM,
        39 => ATTRS_FRAME,
        40 => ATTRS_FRAMESET,
        42..=47 => ATTRS_HEADING,
        49 => ATTRS_HR,
        52 => ATTRS_IFRAME,
        53 => ATTRS_IMG,
        54 => ATTRS_INPUT,
        57 => ATTRS_LABEL,
        58 => ATTRS_LEGEND,
        59 => ATTRS_LI,
        60 => ATTRS_LINK,
        61 => ATTRS_TAG61,
        62 => ATTRS_MAP,
        63 => ATTRS_TAG63,
        65 => ATTRS_META,
        66 => ATTRS_NEXTID,
        71 => ATTRS_OBJECT,
        72 => ATTRS_OL,
        73 => ATTRS_OPTION,
        74 => ATTRS_P,
        75 => ATTRS_PARAM,
        76 => ATTRS_PLAINTEXT,
        84 => ATTRS_SCRIPT,
        85 => ATTRS_SELECT,
        90 => ATTRS_STYLE,
        93 => ATTRS_TABLE,
        94 | 98 | 100 => ATTRS_TBODY,
        95 => ATTRS_TC,
        96 => ATTRS_TD,
        97 => ATTRS_TEXTAREA,
        99 => ATTRS_TD, // <th> uses same attrs as <td>
        102 => ATTRS_TR,
        105 => ATTRS_UL,
        108 => ATTRS_WBR,
        _ => return None,
    };
    lookup(table, code)
}

/// Look up a global HTML attribute by code.
pub(crate) fn html_global_attr(code: u16) -> Option<&'static str> {
    lookup(HTML_GLOBAL_ATTRS, code)
}

// ---------------------------------------------------------------------------
// OPF tag table (43 entries)
// ---------------------------------------------------------------------------

pub static OPF_TAGS: [Option<&str>; 43] = [
    None,                   // 0
    Some("package"),        // 1
    Some("dc:Title"),       // 2
    Some("dc:Creator"),     // 3
    None,                   // 4
    None,                   // 5
    None,                   // 6
    None,                   // 7
    None,                   // 8
    None,                   // 9
    None,                   // 10
    None,                   // 11
    None,                   // 12
    None,                   // 13
    None,                   // 14
    None,                   // 15
    Some("manifest"),       // 16
    Some("item"),           // 17
    Some("spine"),          // 18
    Some("itemref"),        // 19
    Some("metadata"),       // 20
    Some("dc-metadata"),    // 21
    Some("dc:Subject"),     // 22
    Some("dc:Description"), // 23
    Some("dc:Publisher"),   // 24
    Some("dc:Contributor"), // 25
    Some("dc:Date"),        // 26
    Some("dc:Type"),        // 27
    Some("dc:Format"),      // 28
    Some("dc:Identifier"),  // 29
    Some("dc:Source"),      // 30
    Some("dc:Language"),    // 31
    Some("dc:Relation"),    // 32
    Some("dc:Coverage"),    // 33
    Some("dc:Rights"),      // 34
    Some("x-metadata"),     // 35
    Some("meta"),           // 36
    Some("tours"),          // 37
    Some("tour"),           // 38
    Some("site"),           // 39
    Some("guide"),          // 40
    Some("reference"),      // 41
    None,                   // 42
];

// OPF global attributes
pub(super) static OPF_ATTRS: &[(u16, &str)] = &[
    (0x0001, "href"),
    (0x0002, "%never-used"),
    (0x0003, "%guid"),
    (0x0004, "%minimum_level"),
    (0x0005, "%attr5"),
    (0x0006, "id"),
    (0x0007, "href"),
    (0x0008, "media-type"),
    (0x0009, "fallback"),
    (0x000A, "idref"),
    (0x000B, "xmlns:dc"),
    (0x000C, "xmlns:oebpackage"),
    (0x000D, "role"),
    (0x000E, "file-as"),
    (0x000F, "event"),
    (0x0010, "scheme"),
    (0x0011, "title"),
    (0x0012, "type"),
    (0x0013, "unique-identifier"),
    (0x0014, "name"),
    (0x0015, "content"),
    (0x0016, "xml:lang"),
];

/// Look up an OPF attribute by code (global only; no per-tag attrs for OPF).
pub(crate) fn opf_global_attr(code: u16) -> Option<&'static str> {
    lookup(OPF_ATTRS, code)
}

/// OPF has no per-tag attributes.
pub(crate) fn opf_tag_attr(_tag_index: usize, _code: u16) -> Option<&'static str> {
    None
}

// ---------------------------------------------------------------------------
// Map bundle type used by unbinary
// ---------------------------------------------------------------------------

/// A complete set of tag/attribute maps for the unbinary decoder.
pub(crate) struct LitMap {
    pub tags: &'static [Option<&'static str>],
    pub global_attr: fn(u16) -> Option<&'static str>,
    pub tag_attr: fn(usize, u16) -> Option<&'static str>,
}

pub(crate) static HTML_MAP: LitMap = LitMap {
    tags: &HTML_TAGS,
    global_attr: html_global_attr,
    tag_attr: html_tag_attr,
};

pub(crate) static OPF_MAP: LitMap = LitMap {
    tags: &OPF_TAGS,
    global_attr: opf_global_attr,
    tag_attr: opf_tag_attr,
};
