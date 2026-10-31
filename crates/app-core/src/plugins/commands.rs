#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChatCommand {
    pub name: &'static str,
    pub description: &'static str,
    pub insert_text: &'static str,
    pub requires_args: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChatCommandResolution {
    NotCommand,
    Unknown,
    MissingArguments {
        message: String,
        insert_text: &'static str,
    },
    Resolved {
        display: String,
        prompt: String,
        kind: ChatRequestKind,
    },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ChatRequestKind {
    #[default]
    Normal,
    ChapterSummary,
}

pub const CHAT_COMMANDS: [ChatCommand; 4] = [
    ChatCommand {
        name: "/summary",
        description: "总结当前章节内容",
        insert_text: "/summary",
        requires_args: false,
    },
    ChatCommand {
        name: "/search",
        description: "搜索书籍内容并整理答案",
        insert_text: "/search ",
        requires_args: true,
    },
    ChatCommand {
        name: "/rewrite",
        description: "非持久改写当前章节正文",
        insert_text: "/rewrite ",
        requires_args: false,
    },
    ChatCommand {
        name: "/extract",
        description: "提取当前章节关键概念",
        insert_text: "/extract",
        requires_args: false,
    },
];

pub fn chat_command_suggestions(input: &str) -> Vec<ChatCommand> {
    let input = input.trim_start();
    let Some(token) = command_token(input) else {
        return Vec::new();
    };
    let token = token.to_ascii_lowercase();
    let has_args = input
        .get(token.len()..)
        .is_some_and(|suffix| suffix.chars().any(char::is_whitespace));
    if has_args
        && CHAT_COMMANDS
            .iter()
            .any(|command| command.name.eq_ignore_ascii_case(&token))
    {
        return Vec::new();
    }
    CHAT_COMMANDS
        .into_iter()
        .filter(|command| command.name.starts_with(&token))
        .collect()
}

pub fn resolve_chat_command(input: &str) -> ChatCommandResolution {
    let input = input.trim();
    if !input.starts_with('/') {
        return ChatCommandResolution::NotCommand;
    }
    let (name, args) = input
        .split_once(char::is_whitespace)
        .map_or((input, ""), |(name, args)| (name, args.trim()));
    let Some(command) = CHAT_COMMANDS
        .into_iter()
        .find(|command| command.name.eq_ignore_ascii_case(name))
    else {
        return ChatCommandResolution::Unknown;
    };
    if command.requires_args && args.is_empty() {
        let message = match command.name {
            "/search" => "请输入搜索关键词，例如 `/search feedback loops`。",
            _ => "这个技能需要补充参数。",
        };
        return ChatCommandResolution::MissingArguments {
            message: message.into(),
            insert_text: command.insert_text,
        };
    }

    let prompt = match command.name {
        "/summary" => "请用中文总结当前章节。先用一句话概括，再列出关键要点。单独解释重要术语。每个主要结论使用提供的 citation 就近引用。".into(),
        "/search" => format!(
            "用 searchBook 搜索本书中与“{args}”相关的信息。需要完整章节时调用 getContent。用中文列出最相关的章节或段落，并简要解释上下文。"
        ),
        "/rewrite" => {
            let extra = if args.is_empty() {
                String::new()
            } else {
                format!("\n额外改写要求：{args}")
            };
            format!(
                "将当前章节正文改写为通俗易懂的中文。先调用 getContent 获取块 id，再调用 rewriteBlocks 修改正文。保留核心信息、术语和逻辑。不要修改图片或表格。完成后简要说明结果。{extra}"
            )
        }
        "/extract" => "用中文提取当前章节的关键概念。先列出概念，再解释各自的含义、在本章中的作用和相互关系。具体内容使用对应段落的 citation 引用。".into(),
        _ => unreachable!("every registered command has a prompt"),
    };
    ChatCommandResolution::Resolved {
        display: input.to_owned(),
        prompt,
        kind: if command.name == "/summary" {
            ChatRequestKind::ChapterSummary
        } else {
            ChatRequestKind::Normal
        },
    }
}

fn command_token(input: &str) -> Option<&str> {
    input
        .starts_with('/')
        .then(|| input.split_whitespace().next().unwrap_or(input))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suggestions_match_slash_prefix_without_story_memory_commands() {
        let names = chat_command_suggestions("/s")
            .into_iter()
            .map(|command| command.name)
            .collect::<Vec<_>>();
        assert_eq!(names, vec!["/summary", "/search"]);
        assert!(CHAT_COMMANDS.iter().all(|command| !matches!(
            command.name,
            "/story-index" | "/timeline" | "/profile" | "/relations" | "/entities"
        )));
        assert_eq!(chat_command_suggestions("/SUMMARY")[0].name, "/summary");
        assert_eq!(chat_command_suggestions("/summary")[0].name, "/summary");
        assert!(chat_command_suggestions("/summary now").is_empty());
    }

    #[test]
    fn search_requires_arguments_and_expands_to_a_tool_prompt() {
        assert!(matches!(
            resolve_chat_command("/search"),
            ChatCommandResolution::MissingArguments { .. }
        ));
        let ChatCommandResolution::Resolved {
            display, prompt, ..
        } = resolve_chat_command("/SEARCH feedback loops")
        else {
            panic!("command should resolve");
        };
        assert_eq!(display, "/SEARCH feedback loops");
        assert!(prompt.contains("feedback loops"));
        assert!(prompt.contains("searchBook"));
    }

    #[test]
    fn rewrite_expands_to_controlled_document_tools() {
        let ChatCommandResolution::Resolved { prompt, .. } =
            resolve_chat_command("/rewrite 保持英文")
        else {
            panic!("command should resolve");
        };
        assert!(prompt.contains("getContent"));
        assert!(prompt.contains("rewriteBlocks"));
        assert!(prompt.contains("保持英文"));
    }

    #[test]
    fn summary_uses_the_direct_chapter_request_kind() {
        let ChatCommandResolution::Resolved { prompt, kind, .. } = resolve_chat_command("/summary")
        else {
            panic!("command should resolve");
        };
        assert_eq!(kind, ChatRequestKind::ChapterSummary);
        assert!(!prompt.contains("getVisualContent"));
        assert!(prompt.contains("citation"));
        assert!(!prompt.contains("href"));
    }
}
