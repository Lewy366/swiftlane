# Swiftlane's Security-first policy

Security comes before convenience in every part of Swiftlane. Every feature is built and reviewed against these rules:

1. **Least access.** Nothing listens on the network unless a feature needs it, and only as widely as needed: this PC only, your home network, or (for share links) through Tailscale while something is shared.
2. **Nothing without your click.** No silent installs, no automatic changes to Windows, the firewall or your network. Admin rights are asked for one action at a time.
3. **Prove who it is.** Your phone needs the pairing code; share links are random and limited; updates must carry the owner's signature.
4. **Distrust everything from outside.** File names, device names, links and downloads are checked and shown as text, never run.
5. **Your data stays yours.** Files are sent directly between your devices. The only outside services contacted are the ones a feature obviously needs, and the app says which.
6. **Secrets stay out of reach.** Pairing codes and passwords are never shown in logs; the update signing key lives offline, never in this repository.
7. **Honest limits.** If something can't be made safe, Swiftlane says so instead of pretending.
8. **Attack-tested.** New features are tested against misuse (path tricks, guessing, flooding, tampering), not just normal use.

**Found a security problem?** Please don't post it publicly; contact the owner through GitHub (@Lewy366).