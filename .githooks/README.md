# Git hooks

Enable the tracked hooks in a checkout with:

```sh
git config core.hooksPath .githooks
```

The `commit-msg` hook checks the mechanically enforceable commit-message rules:
a concise capitalized subject, no trailing period, a blank line before the
body, and 72-column body text.
