### Fix visibility of SubscriptionTaskParams

We had SubscriptionTaskParams set as pub and folks started using a builder that has it as its argument (the fake_bulder off of execution::Request); with the change in visibility, they weren't able to compile unit tests for their plugins

New docs test (compiled as an external crate so a good test of the public API's visibility) and a couple of WARNs to make sure we don't make that same mistake again

By [@aaronarinder](https://github.com/aaronarinder) in https://github.com/apollographql/router/pull/8771
