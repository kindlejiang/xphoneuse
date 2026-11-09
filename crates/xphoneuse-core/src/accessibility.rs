use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct UiElement {
    pub class: String,
    pub text: String,
    pub content_desc: String,
    pub resource_id: String,
    pub package: String,
    pub clickable: bool,
    pub scrollable: bool,
    pub enabled: bool,
    pub checked: bool,
    pub focused: bool,
    pub center_x: f64,
    pub center_y: f64,
    pub children: Vec<UiElement>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FoundElement {
    #[serde(skip_serializing_if = "String::is_empty")]
    pub text: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub content_desc: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub resource_id: String,
    pub class: String,
    pub clickable: bool,
    pub center_x: f64,
    pub center_y: f64,
}

fn parse_bounds(s: &str) -> Option<(i32, i32, i32, i32)> {
    let parts: Vec<&str> = s.split("][").collect();
    if parts.len() != 2 {
        return None;
    }
    let lt: Vec<i32> = parts[0]
        .trim_start_matches('[')
        .split(',')
        .filter_map(|v| v.parse().ok())
        .collect();
    let rb: Vec<i32> = parts[1]
        .trim_end_matches(']')
        .split(',')
        .filter_map(|v| v.parse().ok())
        .collect();
    if lt.len() == 2 && rb.len() == 2 {
        Some((lt[0], lt[1], rb[0], rb[1]))
    } else {
        None
    }
}

fn parse_node(node: &roxmltree::Node, sw: f64, sh: f64) -> Option<UiElement> {
    if !node.is_element() || node.tag_name().name() != "node" {
        return None;
    }

    let bounds_str = node.attribute("bounds").unwrap_or("");
    let (left, top, right, bottom) = parse_bounds(bounds_str).unwrap_or((0, 0, 0, 0));
    let cx = (left + right) as f64 / 2.0;
    let cy = (top + bottom) as f64 / 2.0;

    let children: Vec<UiElement> = node
        .children()
        .filter_map(|c| parse_node(&c, sw, sh))
        .collect();

    Some(UiElement {
        class: node.attribute("class").unwrap_or("").to_string(),
        text: node.attribute("text").unwrap_or("").to_string(),
        content_desc: node.attribute("content-desc").unwrap_or("").to_string(),
        resource_id: node.attribute("resource-id").unwrap_or("").to_string(),
        package: node.attribute("package").unwrap_or("").to_string(),
        clickable: node.attribute("clickable") == Some("true"),
        scrollable: node.attribute("scrollable") == Some("true"),
        enabled: node.attribute("enabled") == Some("true"),
        checked: node.attribute("checked") == Some("true"),
        focused: node.attribute("focused") == Some("true"),
        center_x: if sw > 0.0 {
            (cx / sw * 1000.0).round() / 1000.0
        } else {
            0.0
        },
        center_y: if sh > 0.0 {
            (cy / sh * 1000.0).round() / 1000.0
        } else {
            0.0
        },
        children,
    })
}

pub fn parse_ui_hierarchy(
    xml: &str,
    screen_width: u32,
    screen_height: u32,
) -> Result<Vec<UiElement>, String> {
    let doc =
        roxmltree::Document::parse(xml).map_err(|e| format!("XML parse error: {}", e))?;
    let root = doc.root_element();
    let sw = screen_width as f64;
    let sh = screen_height as f64;

    Ok(root
        .children()
        .filter_map(|c| parse_node(&c, sw, sh))
        .collect())
}

pub fn format_tree(elements: &[UiElement], indent: usize) -> String {
    let mut out = String::new();
    for el in elements {
        let pad = "  ".repeat(indent);
        let short_class = el.class.rsplit('.').next().unwrap_or(&el.class);

        let mut attrs = Vec::new();
        if !el.text.is_empty() {
            attrs.push(format!("\"{}\"", el.text));
        }
        if !el.content_desc.is_empty() {
            attrs.push(format!("desc=\"{}\"", el.content_desc));
        }
        if !el.resource_id.is_empty() {
            let short = el.resource_id.rsplit('/').next().unwrap_or(&el.resource_id);
            attrs.push(format!("id:{}", short));
        }
        if el.clickable {
            attrs.push("clickable".into());
        }
        if el.scrollable {
            attrs.push("scrollable".into());
        }
        if el.checked {
            attrs.push("checked".into());
        }
        if el.focused {
            attrs.push("focused".into());
        }
        if !el.enabled {
            attrs.push("disabled".into());
        }

        let attr_str = if attrs.is_empty() {
            String::new()
        } else {
            format!(" [{}]", attrs.join(", "))
        };

        out.push_str(&format!(
            "{}{}{} ({:.3}, {:.3})\n",
            pad, short_class, attr_str, el.center_x, el.center_y
        ));

        if !el.children.is_empty() {
            out.push_str(&format_tree(&el.children, indent + 1));
        }
    }
    out
}

fn flatten<'a>(elements: &'a [UiElement], result: &mut Vec<&'a UiElement>) {
    for el in elements {
        result.push(el);
        flatten(&el.children, result);
    }
}

pub fn find_elements(
    elements: &[UiElement],
    text: Option<&str>,
    content_desc: Option<&str>,
    resource_id: Option<&str>,
    class_name: Option<&str>,
    clickable_only: bool,
) -> Vec<FoundElement> {
    let mut flat = Vec::new();
    flatten(elements, &mut flat);

    flat.into_iter()
        .filter(|el| {
            if clickable_only && !el.clickable {
                return false;
            }
            if let Some(t) = text {
                if !el.text.to_lowercase().contains(&t.to_lowercase()) {
                    return false;
                }
            }
            if let Some(cd) = content_desc {
                if !el
                    .content_desc
                    .to_lowercase()
                    .contains(&cd.to_lowercase())
                {
                    return false;
                }
            }
            if let Some(rid) = resource_id {
                if !el
                    .resource_id
                    .to_lowercase()
                    .contains(&rid.to_lowercase())
                {
                    return false;
                }
            }
            if let Some(cn) = class_name {
                if !el.class.to_lowercase().contains(&cn.to_lowercase()) {
                    return false;
                }
            }
            true
        })
        .map(|el| FoundElement {
            text: el.text.clone(),
            content_desc: el.content_desc.clone(),
            resource_id: el.resource_id.clone(),
            class: el
                .class
                .rsplit('.')
                .next()
                .unwrap_or(&el.class)
                .to_string(),
            clickable: el.clickable,
            center_x: el.center_x,
            center_y: el.center_y,
        })
        .collect()
}
