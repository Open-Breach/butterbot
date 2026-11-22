# Butterbot

Certificates are the bread and butter of the internet: everything runs on them, and looking after them is boring. Butterbot does the boring part.

It is a small program that runs once a day, checks every certificate you have told it about, and gets a new one from an ACME certificate authority (such as Let's Encrypt) when one is missing or about to expire. It proves ownership through DNS and delivers the result to wherever your services pick certificates up.

Everything it talks to is a named provider in the configuration:

- **ACME servers**: where certificates come from.
- **DNS providers**: where the validation records live.
- **Certificate stores**: where certificates go.

Each certificate says which of each it uses, so one Butterbot can serve several certificate authorities, DNS hosts and destinations.

## Status

Design stage. There is no code yet. The first version is specified as an [OpenSpec](https://github.com/Fission-AI/OpenSpec) change:

- [Proposal](openspec/changes/add-certificate-maintainer/proposal.md): what and why
- [Design](openspec/changes/add-certificate-maintainer/design.md): how, and what is still open
- [Specs](openspec/changes/add-certificate-maintainer/specs/): the behaviour, one file per capability
- [Tasks](openspec/changes/add-certificate-maintainer/tasks.md): the implementation plan
