假设你要严格按照下面这份 PRD 实现，不能问任何人。
列出缺失的内容：文件路径、数据结构、接口 / 函数签名、错误处理、验收标准。
也检查里程碑（或阶段）怎么切：每个阶段做完后能否运行、能否验证、有没有明确的验收标准；
缺了归入 `acceptance`。
最后给出 GO 或 NO-GO 和理由。

输出要求：
- `missing`：每条标明类别 `kind`（path / data / interface / error / acceptance）、章节、缺什么、严重度。没有就输出空数组。
- `verdict`：只有在不需要猜任何会影响实现的内容时才给 GO。
- `reason`：一两句话说明结论。

## 审稿规则

{{goldfish_rules}}

## PRD

{{prd}}
