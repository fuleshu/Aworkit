# Ready-made workflows

This folder holds four example workflows you can add to Aworkit. Each one is a
small recipe that tells Aworkit how to handle a request: which helper should
answer, when to look things up, when to ask you first, and so on.

They are here for two reasons:

- to show what the workflow builder can do, and
- to be genuinely useful as they are.

You can use them unchanged, or import one and tweak it to your liking.

## How to add one

In the workflow editor, choose **Import** and pick one of the `.aworkit.json`
files in this folder. That's it — the workflow appears in your list and you can
select it like any other.

A brand-new Aworkit profile already includes three ready-to-use workflows of
its own — **Simple**, **Standard** and **Planer** — and starts on *Standard*.
The four workflows in this folder are extras you import when you want them;
none of them are switched on for you.

*Delegated Code Review* needs some setup first (see its section below), so
import that one only when you're ready for it.

---

## 1. Triage Router

**In one sentence:** It answers easy questions with a quick helper, and only
brings in the heavier, web-connected helper — with your permission — when a
question really needs it.

**What you'll experience**

- You ask something ordinary, like *"How many calories are in a banana?"* or
  *"What's the capital of France?"*. A fast helper answers straight away. No
  searching, no waiting, no cost of a big lookup.
- You ask something that depends on the world right now, like *"What's the
  weather in Berlin this afternoon?"*. Aworkit pauses and asks you to approve
  using the full helper. Approve it and it looks things up and answers with
  sources. Decline and the run stops.
- It judges only your latest message. An earlier question that needed the web
  won't make later simple questions slow.

**Best for:** everyday chatting where you don't want every question to turn into
a web search.

---

## 2. Evidence Brief

**In one sentence:** It writes short, sourced answers — and skips the research
when the answer doesn't actually need it.

**What you'll experience**

- It starts looking for web sources in the background while it works out what
  the question really needs.
- If outside facts matter, a research helper checks the details, uses live pages
  where it must, and gives you a short brief with links you can click.
- If the question is plain general knowledge, a quick helper just answers it.
  No unnecessary searching.
- When it's done, the conversation waits for your next message.

**Best for:** questions where you want a trustworthy answer with sources.

**Good to know:** It uses web search, which works out of the box with the
built-in search. Nothing to configure.

---

## 3. Iterative Planning

**In one sentence:** It makes a plan, improves it until there are no open
questions left, then carries it out.

**What you'll experience**

- Aworkit first sketches a plan for your request and notes anything still
  unclear.
- If questions remain, it goes back and sharpens the plan — up to three rounds.
- Once the plan is settled, a working helper follows it and produces the answer.
- If the plan still isn't perfectly settled after three rounds, it goes ahead
  with the best version it has and tells you that's what happened. You're never
  left waiting forever.

**Best for:** larger requests where a little thinking up front noticeably
improves the result.

---

## 4. Delegated Code Review

**In one sentence:** It hands a review of your project to a second, independent
AI helper.

**What you'll experience**

- It scans your project for leftover notes developers leave behind, like
  `TODO`, `FIXME` and `HACK`.
- Before spending anything, it asks you to approve the review.
- On approval, it sends those findings to an outside coding helper (Codex or
  Claude Code), which looks at the files and reports real risks, likely bugs,
  and missing tests — each with a file path.

**Setup required:** You first need to connect a Codex or Claude Code helper in
Settings. Without one, this workflow can't run. That's exactly why it isn't
switched on by default — import it once your helper is ready.

**Best for:** getting a fresh, independent opinion on a codebase before you
ship.

---

## Which one should I reach for?

| If you want… | Use |
| --- | --- |
| Everyday answers, fast, without needless searching | **Triage Router** |
| A sourced answer on a topic that needs checking | **Evidence Brief** |
| More care and planning before the answer | **Iterative Planning** |
| An outside opinion on your code | **Delegated Code Review** |

## A few honest notes

- These are examples, not rules. Import one, open it in the editor, and change
  the wording of any step — the prompts are written in plain sentences on
  purpose.
- The text you can edit lives on each step. For instance, you can make the
  Triage Router stricter about what counts as "needs the web", or tell the
  Evidence Brief to always cite at least two sources.
- Nothing here phones home or spends money without either the built-in free
  search or the helper you approved.
- *Delegated Code Review* is the only one that needs extra setup; the other
  three work as soon as you import them.
