// Whole-token matches only: short grammar words must not remove identifiers such as ONNX or login.
const ENGLISH_STOP_WORDS = `
  a an the and or but nor so yet for as at by from in into of off on onto out over per
  to up upon via with within without about above across after against along amid among
  around before behind below beneath beside besides between beyond down during except
  inside near outside past since than through throughout till toward towards under
  underneath until versus vs whereas whether while
  am is are was were be been being do does did doing done have has had having
  can cannot could may might must shall should will would ought need needs
  i me my mine myself we us our ours ourselves you your yours yourself yourselves
  he him his himself she her hers herself it its itself they them their theirs themselves
  this that these those who whom whose which what whatever whoever whomever
  all any anybody anyone anything both each either everybody everyone everything
  few many more most much neither nobody none noone nothing one ones other others
  several some somebody someone something such same own another enough
  not no yes only also even ever never always already still just very too quite rather
  almost really often sometimes usually again once twice then now here there
  therefore thus hence however moreover furthermore otherwise nonetheless nevertheless
  how why when whenever where wherever please else if unless because although though
  despite instead indeed perhaps maybe probably possibly simply actually basically
  able allow allows allowed allowing get gets getting got make makes making made
  use uses using used user users prefer prefers want wants like let lets
  default current some such somewhen somewhat somehow somewhere anywhere everywhere
  don doesn didn isn aren wasn weren hasn haven hadn won wouldn shouldn couldn
  ain mustn mightn needn shan ll re ve wouldn shouldnt couldnt didnt doesnt isnt arent
  wasnt werent havent hasnt hadnt wont dont im ive youre theyre thats theres
`

const CHINESE_STOP_WORDS = `
  用户 使用 需要 可以 进行 通过 一个 或者 以及 如果 当前 默认 记忆
  支持 不要 不能 要求 没有 已经 自己 我们 他们 这个 那个 这些 那些 什么 怎么 如何 因为 所以
  但是 而且 同时 相关 对于 关于 其中 之后 之前 时候 还是 不是 不是的 作为 所有 每个 一些
  一种 一次 一条 这样 那样 比较 非常 可能 应该 必须 能够 只要 只有 还有 然后 现在
  直接 确认 建议 希望 保持 优先 统一 完成 处理 操作 内容 信息 记录 添加 删除 修改 更新
  例如 比如 主要 实际 对应 结果 方式 问题 方案 工作 任务 项目 版本 返回 查看 显示
  你们 咱们 它们 他的 她的 它的 我的 你的 我们的 你们的 他们的
  这是 那是 这里 那里 之中 之间 之内 之外 以内 以外 以上 以下
  由于 于是 因此 不过 然而 并且 或是 还是 及其 以及 即使 即便 无论 不论
  有些 有的 有关 此外 另外 其他 其它 某个 某些 各个 各种 各自
  一般 通常 总是 一直 仍然 仍旧 目前 当时 随后 最后 首先 其次 再次
`

export const KEYWORD_STOP_WORDS: ReadonlySet<string> = new Set(
  `${ENGLISH_STOP_WORDS} ${CHINESE_STOP_WORDS}`.trim().split(/\s+/),
)
