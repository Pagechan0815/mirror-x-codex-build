# Key registration

Use this only after the user directly supplies a 镜子AI Image Key.

Pass the Key to the Skill's `jingzi-imagegen` wrapper using the `register-key --stdin` action. Send it through standard input, never as a command-line argument, and do not print or repeat it. The registered credential is stored in the Skill's independent local configuration and is not added to Codex `config.toml`.

After registration succeeds, continue the user's original image request. If validation reports that `gpt-image-2` is unavailable, explain that the Key must belong to the 镜子AI image group and leave any previously registered Key unchanged.
