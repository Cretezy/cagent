---
name: skill-creator
description: Guide for creating effective Cagent skills. Use when users want to create a new skill or update an existing skill with specialized knowledge, workflows, scripts, references, or assets.
---

# Skill Creator

Skills are self-contained directories that progressively disclose task-specific instructions.
Every skill requires `SKILL.md`; it may also contain `scripts/`, `references/`, and `assets/`.

Keep instructions concise. Assume Cagent already understands general software work and include only
non-obvious procedures, domain facts, reusable resources, or fragile command sequences. Put detailed
material in references and tell the agent exactly when to read it. Scripts remain subject to the
normal tool and permission policy.

## Required format

```markdown
---
name: example-skill
description: What the skill does and specific situations that should activate it.
---

# Example Skill

Instructions...
```

`description` is required and is the activation mechanism. `name` should be lowercase hyphen-case
and normally match the directory; when omitted, Cagent uses the directory name. Put all activation
criteria in the description because the body is read only after activation.

## Workflow

1. Clarify the skill's recurring use cases and gather representative examples.
2. Ask where to create it if the user did not specify. Default to the effective Cagent config
   directory's `skills/` folder so Cagent discovers it automatically. With the normal Unix config,
   this is `${XDG_CONFIG_HOME:-$HOME/.config}/cagent/skills`.
3. Create the directory and `SKILL.md`; add only resources the workflow needs.
5. Test the skill against realistic prompts and tighten ambiguous or verbose instructions.

Do not create files under `.system`; Cagent replaces that directory when its bundled fingerprint
changes. Project-specific skills may instead live in `<workspace>/skills` or
`<workspace>/.cagent/skills`.
