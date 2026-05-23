//! Dev-time parity tool. Reads PromptParams JSON on stdin, calls
//! `engine::prompt::assemble_prompt`, and prints the AssembledPrompt as
//! JSON on stdout. The TS daemon's `scripts/parity-check-prompt.ts`
//! pipes the same fixtures through both ports and diffs the outputs.
//!
//! Build:  cargo build --release -p shore-daemon --example dump_assemble_prompt
//! Run:    cat fixture.json | ./target/release/examples/dump_assemble_prompt

use std::io::Read;

use serde::{Deserialize, Serialize};
use shore_daemon::engine::prompt::{
    AssembledPrompt, PromptMessage, PromptParams, SystemBlock, assemble_prompt,
};
use shore_protocol::types::{ContentBlock, ImageRef, Message, Role};

#[derive(Deserialize)]
struct InputParams {
    character_name: String,
    display_name: String,
    #[serde(default)]
    system_prompt: Option<String>,
    #[serde(default)]
    tools_guidance: Option<String>,
    #[serde(default)]
    character_definition: Option<String>,
    #[serde(default)]
    user_definition: Option<String>,
    #[serde(default)]
    memory_index: Option<String>,
    is_private: bool,
    has_prior_context: bool,
    messages: Vec<Message>,
    #[serde(default)]
    max_context_tokens: Option<u32>,
    #[serde(default)]
    max_output_tokens: Option<u32>,
}

#[derive(Serialize)]
struct OutSystemBlock<'a> {
    label: &'a str,
    content: &'a str,
}

#[derive(Serialize)]
struct OutPromptMessage<'a> {
    role: &'a Role,
    content: &'a str,
    images: &'a [ImageRef],
    content_blocks: &'a [ContentBlock],
}

#[derive(Serialize)]
struct OutAssembled<'a> {
    system: Vec<OutSystemBlock<'a>>,
    messages: Vec<OutPromptMessage<'a>>,
}

fn main() {
    let mut buf = String::new();
    std::io::stdin()
        .read_to_string(&mut buf)
        .expect("failed to read stdin");
    let input: InputParams = serde_json::from_str(&buf).expect("invalid JSON on stdin");

    let params = PromptParams {
        character_name: &input.character_name,
        display_name: &input.display_name,
        system_prompt: input.system_prompt.as_deref(),
        tools_guidance: input.tools_guidance.as_deref(),
        character_definition: input.character_definition.as_deref(),
        user_definition: input.user_definition.as_deref(),
        memory_index: input.memory_index.as_deref(),
        is_private: input.is_private,
        has_prior_context: input.has_prior_context,
        messages: &input.messages,
        max_context_tokens: input.max_context_tokens,
        max_output_tokens: input.max_output_tokens,
    };

    let result: AssembledPrompt = assemble_prompt(&params);
    let out = to_serializable(&result);
    let s = serde_json::to_string(&out).expect("failed to serialize");
    println!("{s}");
}

fn to_serializable(p: &AssembledPrompt) -> OutAssembled<'_> {
    OutAssembled {
        system: p
            .system
            .iter()
            .map(|b: &SystemBlock| OutSystemBlock {
                label: &b.label,
                content: &b.content,
            })
            .collect(),
        messages: p
            .messages
            .iter()
            .map(|m: &PromptMessage| OutPromptMessage {
                role: &m.role,
                content: &m.content,
                images: &m.images,
                content_blocks: &m.content_blocks,
            })
            .collect(),
    }
}
