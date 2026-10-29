#import "chapter.typ": chapter

#set page(margin: 2cm, numbering: "1")
#set text(size: 11pt, font: "Noto Sans SC")
#set heading(numbering: "1.")

#figure(
  placement: auto,
  alt: "12345",
  [q],
  caption: "123"
)
#grid(
  columns: (1fr,2fr),
  grid.cell(
    fill: black,
    inset: 0pt,
    x:0,
    [123]
  )
)
#grid.cell(
  [
    123
    - 123
    - abc
  ]
)

= Typst Editor 测试文档

这是一段*中文*测试文本，#strong[用于]验证语法高亮、字数统计与矢量预览。The quick brown fox
jumps over the lazy dog.

== 功能清单

- 项目树与主文件（右键设为 main file）
- 语法高亮 + 红色波浪线错误提示
- 矢量预览（vello GPU 渲染，任意缩放清晰）
- 双击预览跳转到源码 / 右键编辑器"在预览区中显示"圆环
  - _123_
  - 123
- 123
#chapter("第一章")

== 数学公式

$ sum_(i=1)^n i = (n(n+1))/2 $

== 代码块

```rust
fn main() {
    println!("hello typst editor");
}
```
== 表格

#table(
  columns: 3,
  [
    *名称*
  ], [
    状态
  ], 
  [
    备注
  ],
  [预览], [矢量], [vello],
  [编辑器], [egui], [波浪线],
)
