### Add the ability to rename metrics ([Issue #ISSUE_NUMBER](https://github.com/apollographql/router/issues/ISSUE_NUMBER))

Add ability to rename instruments via opentelemetry views

Value adds:
- **Costs**: Some observability platforms only allow tag indexing controls ($$$$) on a per metric name basis.  Use of otlp semantic naming conventions and having the same metric name emanated by different services can prevent effective use of these controls. 
- **Conventions**: Many customers have specific metric naming conventions across their organization, this allows them to align with said conventions. 

By [Jon Christiansen](https://github.com/theJC) in https://github.com/apollographql/router/pull/8412
