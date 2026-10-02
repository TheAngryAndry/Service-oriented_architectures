//! Render the C4 model with fixed lanes. Requires Graphviz and rsvg-convert.
//! Run: cargo run --manifest-path scripts/Cargo.toml --bin render-architecture
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Write,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

type Point = (i32, i32);
type Rect = (i32, i32, i32, i32);
type Edge = (String, String, Value);
type Route = (Vec<Point>, Point);

fn escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
fn label(svg: &mut String, x: i32, y: i32, value: &str, size: i32, color: &str, anchor: &str) {
    write!(svg, "<text x='{x}' y='{y}' font-family='Arial' font-size='{size}' fill='{color}' text-anchor='{anchor}'>").unwrap();
    for (i, line) in value.replace("\\n", "\n").lines().enumerate() {
        let dy = if i == 0 { 0 } else { size + 5 };
        write!(svg, "<tspan x='{x}' dy='{dy}'>{}</tspan>", escape(line)).unwrap();
    }
    svg.push_str("</text>");
}
fn wrapped(value: &str) -> String {
    let mut result = Vec::new();
    for line in value.lines() {
        let mut current = String::new();
        for word in line.split_whitespace() {
            if !current.is_empty() && current.chars().count() + 1 + word.chars().count() > 40 {
                result.push(std::mem::take(&mut current));
            }
            if !current.is_empty() {
                current.push(' ');
            }
            current.push_str(word);
        }
        if !current.is_empty() {
            result.push(current);
        }
    }
    result.join("\n")
}
fn rasterize(path: &Path) {
    assert!(
        Command::new("rsvg-convert")
            .arg("--zoom=2")
            .arg("-o")
            .arg(path.with_extension("png"))
            .arg(path)
            .status()
            .expect("rsvg-convert is required")
            .success()
    );
}
fn view(
    name: &str,
    title: &str,
    edges: &[Edge],
    nodes: &BTreeMap<String, Value>,
    events: bool,
) -> (String, i32) {
    let height = if events { 900 } else { 1430 };
    let mut svg = String::from("<rect width='2200' height='100%' fill='white'/>");
    label(&mut svg, 32, 38, title, 27, "#153D5A", "start");
    let mut positions: BTreeMap<String, Rect> = BTreeMap::new();
    let mut routes: BTreeMap<(String, String), Route> = BTreeMap::new();
    let mut route = |a: &str, b: &str, points: &[Point], label: Point| {
        routes.insert((a.into(), b.into()), (points.to_vec(), label));
    };
    let boundary = if events {
        for (n, r) in [
            ("catalog", (75, 130, 350, 120)),
            ("users", (75, 360, 350, 120)),
            ("orders", (75, 630, 350, 145)),
            ("broker", (910, 340, 390, 180)),
            ("feed", (1760, 130, 350, 120)),
            ("payments", (1760, 360, 350, 120)),
            ("notifications", (1760, 650, 350, 120)),
        ] {
            positions.insert(n.into(), r);
        }
        route(
            "catalog",
            "broker",
            &[(425, 190), (715, 190), (910, 375)],
            (650, 145),
        );
        route("users", "broker", &[(425, 420), (910, 420)], (650, 380));
        route(
            "orders",
            "broker",
            &[(425, 660), (690, 660), (910, 475)],
            (650, 575),
        );
        route(
            "broker",
            "orders",
            &[(970, 520), (970, 745), (425, 745)],
            (720, 705),
        );
        route(
            "broker",
            "feed",
            &[(1300, 375), (1500, 190), (1760, 190)],
            (1580, 120),
        );
        route(
            "broker",
            "payments",
            &[(1300, 420), (1760, 420)],
            (1520, 375),
        );
        route(
            "payments",
            "broker",
            &[(1935, 480), (1935, 565), (1210, 565), (1210, 520)],
            (1580, 525),
        );
        route(
            "broker",
            "notifications",
            &[(1105, 520), (1105, 710), (1760, 710)],
            (1450, 665),
        );
        (24, 70, 2152, 745)
    } else {
        for (n, r) in [
            ("buyer", (32, 120, 300, 110)),
            ("seller", (32, 310, 300, 110)),
            ("web", (430, 215, 300, 110)),
            ("gateway", (430, 540, 300, 110)),
            ("psp", (1860, 930, 310, 112)),
            ("email", (1860, 730, 310, 112)),
        ] {
            positions.insert(n.into(), r);
        }
        for (i, service) in [
            "catalog",
            "orders",
            "users",
            "notifications",
            "payments",
            "feed",
        ]
        .iter()
        .enumerate()
        {
            let y = 130 + i as i32 * 200;
            positions.insert((*service).into(), (1030, y, 320, 112));
            positions.insert(format!("{service}_db"), (1450, y, 320, 112));
        }
        route(
            "buyer",
            "web",
            &[(332, 175), (375, 175), (430, 248)],
            (225, 95),
        );
        route(
            "seller",
            "web",
            &[(332, 365), (375, 365), (430, 292)],
            (225, 465),
        );
        route("web", "gateway", &[(580, 325), (580, 540)], (650, 420));
        route(
            "orders",
            "catalog",
            &[(1190, 330), (1190, 242)],
            (1390, 267),
        );
        route("orders", "users", &[(1190, 442), (1190, 530)], (1370, 478));
        route(
            "notifications",
            "users",
            &[(1190, 730), (1190, 642)],
            (1370, 678),
        );
        route(
            "notifications",
            "email",
            &[
                (1270, 842),
                (1270, 880),
                (1830, 880),
                (1830, 786),
                (1860, 786),
            ],
            (1550, 857),
        );
        route(
            "payments",
            "psp",
            &[
                (1270, 1042),
                (1270, 1080),
                (1830, 1080),
                (1830, 986),
                (1860, 986),
            ],
            (1540, 1056),
        );
        route(
            "psp",
            "gateway",
            &[(2015, 1042), (2015, 1340), (580, 1340), (580, 650)],
            (1510, 1304),
        );
        for service in ["catalog", "orders", "users", "payments", "feed"] {
            let y = positions[service].1 + 56;
            route(
                "gateway",
                service,
                &[(730, 595), (790, y), (1030, y)],
                (906, y - 44),
            );
        }
        for service in [
            "catalog",
            "orders",
            "users",
            "notifications",
            "payments",
            "feed",
        ] {
            let y = positions[service].1 + 56;
            route(
                service,
                &format!("{service}_db"),
                &[(1350, y), (1450, y)],
                (1400, y - 57),
            );
        }
        (380, 70, 1430, 1220)
    };
    let used: BTreeSet<_> = edges
        .iter()
        .flat_map(|(a, b, _)| [a.clone(), b.clone()])
        .collect();
    assert_eq!(
        used,
        positions.keys().cloned().collect(),
        "Update node positions"
    );
    assert_eq!(
        edges
            .iter()
            .map(|(a, b, _)| (a.clone(), b.clone()))
            .collect::<BTreeSet<_>>(),
        routes.keys().cloned().collect(),
        "Update edge routes"
    );
    let (x, y, w, h) = boundary;
    write!(svg, "<rect x='{x}' y='{y}' width='{w}' height='{h}' rx='18' fill='none' stroke='#ADBCCA' stroke-dasharray='7 5' stroke-width='1.5'/>").unwrap();
    label(
        &mut svg,
        x + 22,
        y + 32,
        "Marketplace [Software System Boundary]",
        17,
        "#52667A",
        "start",
    );
    svg.push_str("<defs>");
    for (kind, color) in [("sync", "#64748B"), ("async", "#80609D")] {
        write!(svg, "<marker id='{name}-{kind}' viewBox='0 0 10 10' refX='9' refY='5' markerWidth='7' markerHeight='7' orient='auto'><path d='M 0 0 L 10 5 L 0 10 z' fill='{color}'/></marker>").unwrap();
    }
    svg.push_str("</defs>");
    for (a, b, edge) in edges {
        let points = routes[&(a.clone(), b.clone())]
            .0
            .iter()
            .map(|(x, y)| format!("{x},{y}"))
            .collect::<Vec<_>>()
            .join(" ");
        let asynchronous = edge["style"] == "dashed";
        let (color, kind, dash) = if asynchronous {
            ("#80609D", "async", "stroke-dasharray='7 5'")
        } else {
            ("#64748B", "sync", "")
        };
        let description = escape(&format!(
            "{a} → {b}: {}",
            edge["label"].as_str().unwrap().replace("\\n", " ")
        ));
        write!(svg, "<polyline points='{points}' fill='none' stroke='{color}' stroke-width='1.8' stroke-linejoin='round' marker-end='url(#{name}-{kind})' {dash}><title>{description}</title></polyline>").unwrap();
    }
    for (a, b, edge) in edges {
        let (x, y) = routes[&(a.clone(), b.clone())].1;
        let mut value = edge["label"].as_str().unwrap().to_owned();
        if b.ends_with("_db") {
            value = value.replace("Читает / пишет", "Читает /\\nпишет");
        }
        if a == "gateway" && b == "payments" {
            value = value.replace("; webhook", ";\\nwebhook");
        }
        label(
            &mut svg,
            x,
            y,
            &value,
            15,
            if edge["style"] == "dashed" {
                "#604478"
            } else {
                "#334155"
            },
            "middle",
        );
    }
    for (node, (x, y, w, h)) in positions {
        let database = node.ends_with("_db");
        let external = ["buyer", "seller", "psp", "email"].contains(&node.as_str());
        let fill = if external {
            "#F1F3F5"
        } else if database {
            "#F6FAFD"
        } else if node == "broker" {
            "#EEE8F6"
        } else {
            "#EAF3FA"
        };
        let stroke = if node == "broker" {
            "#9B7BB7"
        } else if node == "orders" {
            "#24729F"
        } else if external {
            "#BBC4CC"
        } else {
            "#B8CFE0"
        };
        if database {
            write!(svg, "<path d='M{x},{} A{},14 0 0 1 {},{} L{},{} A{},14 0 0 1 {x},{} Z' fill='{fill}' stroke='{stroke}' stroke-width='1.5'/><ellipse cx='{}' cy='{}' rx='{}' ry='14' fill='{fill}' stroke='{stroke}' stroke-width='1.5'/>", y+14,w/2,x+w,y+14,x+w,y+h-14,w/2,y+h-14,x+w/2,y+14,w/2).unwrap();
        } else {
            let sw = if node == "orders" { "2.5" } else { "1.5" };
            write!(svg,"<rect x='{x}' y='{y}' width='{w}' height='{h}' rx='12' fill='{fill}' stroke='{stroke}' stroke-width='{sw}'/>").unwrap();
        }
        let value = nodes[&node]["label"].as_str().unwrap().replace("\\n", "\n");
        let lines: Vec<_> = value.lines().collect();
        label(
            &mut svg,
            x + w / 2,
            y + if database { 41 } else { 29 },
            lines[0],
            20,
            "#153D5A",
            "middle",
        );
        label(
            &mut svg,
            x + w / 2,
            y + if database { 63 } else { 53 },
            lines[1],
            14,
            "#52667A",
            "middle",
        );
        label(
            &mut svg,
            x + w / 2,
            y + if database { 84 } else { 78 },
            &wrapped(&lines[2..].join("\n")),
            15,
            "#153D5A",
            "middle",
        );
    }
    let note = if events {
        "Те же сервисы, что в виде 01. HTTP-вызовы и хранилища здесь не повторяются."
    } else {
        "Событийные связи вынесены в вид 02; асинхронный HTTPS webhook провайдера показан здесь."
    };
    label(&mut svg, 32, height - 38, note, 17, "#52667A", "start");
    (svg, height)
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let directory = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../docs/architecture");
    let output = Command::new("dot")
        .arg("-Tjson")
        .arg(directory.join("container.dot"))
        .output()?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).into_owned().into());
    }
    let model: Value = serde_json::from_slice(&output.stdout)?;
    let objects: BTreeMap<_, _> = model["objects"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| (o["_gvid"].as_u64().unwrap(), o.clone()))
        .collect();
    let nodes: BTreeMap<_, _> = objects
        .values()
        .filter(|o| o.get("label").is_some() && o.get("nodes").is_none())
        .map(|o| (o["name"].as_str().unwrap().to_owned(), o.clone()))
        .collect();
    let edges: Vec<Edge> = model["edges"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["style"] != "invis")
        .map(|e| {
            (
                objects[&e["tail"].as_u64().unwrap()]["name"]
                    .as_str()
                    .unwrap()
                    .into(),
                objects[&e["head"].as_u64().unwrap()]["name"]
                    .as_str()
                    .unwrap()
                    .into(),
                e.clone(),
            )
        })
        .collect();
    let used: BTreeSet<_> = edges
        .iter()
        .flat_map(|(a, b, _)| [a.clone(), b.clone()])
        .collect();
    assert_eq!(
        used,
        nodes.keys().filter(|n| *n != "legend").cloned().collect()
    );
    let mut combined = String::from(
        "<svg xmlns='http://www.w3.org/2000/svg' width='2264' height='2642' viewBox='0 0 2264 2642' role='img' aria-label='Маркетплейс: C4 Container'><rect width='100%' height='100%' fill='white'/>",
    );
    label(
        &mut combined,
        32,
        40,
        "Маркетплейс — C4 Container · Level 2",
        30,
        "#153D5A",
        "start",
    );
    label(
        &mut combined,
        32,
        74,
        "Два вида одной архитектуры. Сервисы с одинаковыми именами — те же контейнеры.",
        20,
        "#475569",
        "start",
    );
    label(
        &mut combined,
        32,
        103,
        "Целевая модель; реализован только каркас Orders с /health.",
        18,
        "#475569",
        "start",
    );
    let mut y = 116;
    for (name, title, events) in [
        ("requests", "01 · Запросы и приватные данные", false),
        ("events", "02 · Асинхронные события через RabbitMQ", true),
    ] {
        let selected: Vec<_> = edges
            .iter()
            .filter(|(a, b, _)| (a == "broker" || b == "broker") == events)
            .cloned()
            .collect();
        let (content, height) = view(name, title, &selected, &nodes, events);
        let path = directory.join(format!("container-{name}.svg"));
        fs::write(
            &path,
            format!(
                "<svg xmlns='http://www.w3.org/2000/svg' width='2200' height='{height}' viewBox='0 0 2200 {height}' role='img' aria-label='{}'>{content}</svg>",
                escape(title)
            ),
        )?;
        rasterize(&path);
        write!(
            combined,
            "<svg x='32' y='{y}' width='2200' height='{height}' viewBox='0 0 2200 {height}'>{content}</svg>"
        )?;
        y += height;
        if !events {
            label(
                &mut combined,
                32,
                y + 38,
                "Сплошная стрелка — синхронный запрос; пунктир — асинхронное сообщение / callback.",
                18,
                "#475569",
                "start",
            );
            label(
                &mut combined,
                32,
                y + 67,
                "Стрелки показывают направление запроса или доставки события. Ответы не показаны.",
                18,
                "#475569",
                "start",
            );
            y += 100;
        }
    }
    label(
        &mut combined,
        32,
        y + 38,
        "Виды дополняют друг друга: HTTP и БД показаны только в 01, связи RabbitMQ — только в 02.",
        18,
        "#475569",
        "start",
    );
    label(
        &mut combined,
        32,
        y + 68,
        "Светло-синий — приложение · цилиндр — приватная БД · фиолетовый — брокер · серый — внешние участники.",
        18,
        "#475569",
        "start",
    );
    combined.push_str("</svg>");
    let path = directory.join("container.svg");
    fs::write(&path, combined)?;
    rasterize(&path);
    println!(
        "Rendered {} elements and {} relationships.",
        used.len(),
        edges.len()
    );
    Ok(())
}
