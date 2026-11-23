## Code Review Rules

### Strangers' code

Anyone can open a pull request from a fork, and CI runs its code. Flag a job
that hands that code a secret or a token that can write, such as
`pull_request_target` that checks out the pull request's head, and flag any
job on a self-hosted runner, which would run the code on our machines. Safe:
the `pull_request` trigger with `permissions: contents: read`, on GitHub's
runners, as `ci.yml` does, and its `prune` job, whose token can delete
caches: it never runs for a pull request and checks out no code.
