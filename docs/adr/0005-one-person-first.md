# One person per machine first, without ruling out more

The first version supports one person per machine: one owner, one set of intents, one Environment, one Data layer, one Agent. Several people sharing a machine needs a shared Base with a separate Environment, Data layer, and Agent for each person. That adds a lot of complexity and doesn't test the thesis, so we defer it.

We still must not make it impossible later. Intents, Environment, Data, the Agent, handles, and ledger entries are always tied to an explicit person, never assumed to be "the" user. Base stays separate from all of them, and the owner of a machine stays a separate concept from the people using it.
