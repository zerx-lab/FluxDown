import { defineMessages } from "../define";

/** QQ 群(`/qq-group/`)与 Telegram 群(`/telegram-group/`)落地页文案。 */
export const groups = defineMessages("groups", {
  en: {
    qq: {
      meta: {
        title: "QQ Group — FluxDown",
        description: "Join the FluxDown QQ community group. Group number: 832143651.",
      },
      eyebrow: "QQ community",
      title: "Join the <em>community</em>",
      lede: "Scan the QR code or search the group number to join the FluxDown QQ group and chat with the developer and other users.",
      qrAlt: "QR code for the FluxDown QQ group",
      figCaption: "scan to join",
      groupNumber: "Group number",
      copy: "Copy",
      copied: "Copied",
      copyLabel: "Copy group number",
      howToJoin: "How to join",
      steps: [
        "Open QQ and scan the QR code to join directly.",
        "Or search for group number 832143651 in QQ.",
        "Apply to join and wait for approval.",
      ],
    },
    telegram: {
      meta: {
        title: "Telegram Group — FluxDown",
        description: "Join the FluxDown Telegram community group for users worldwide.",
      },
      eyebrow: "Telegram community",
      title: "Join the <em>Telegram group</em>",
      lede: "Join the FluxDown global community on Telegram and chat with the developer and users from around the world.",
      join: "Join Telegram group",
      figCaption: "invite link",
      howToJoin: "How to join",
      steps: [
        "Click the button to open the Telegram invite link.",
        "Open it in the Telegram app or on the web — no group search needed.",
        "Tap “Join Group” to become part of the community.",
      ],
    },
  },
  zh: {
    qq: {
      meta: {
        title: "QQ 群 — FluxDown",
        description: "加入 FluxDown QQ 交流群，群号：832143651。",
      },
      eyebrow: "QQ 社区群",
      title: "加入<em>社区群</em>",
      lede: "扫描二维码或搜索群号加入 FluxDown QQ 交流群，与开发者和其他用户一起讨论。",
      qrAlt: "FluxDown QQ 群二维码",
      figCaption: "扫码加群",
      groupNumber: "群号",
      copy: "复制",
      copied: "已复制",
      copyLabel: "复制群号",
      howToJoin: "如何加入",
      steps: [
        "打开 QQ，扫描二维码直接加群。",
        "或在 QQ 中搜索群号 832143651。",
        "点击申请加入，等待审核通过即可。",
      ],
    },
    telegram: {
      meta: {
        title: "Telegram 群 — FluxDown",
        description: "加入 FluxDown Telegram 国际社区群。",
      },
      eyebrow: "Telegram 社区群",
      title: "加入 <em>Telegram 群</em>",
      lede: "加入 FluxDown 国际社区，与全球开发者和用户一起交流讨论。",
      join: "加入 Telegram 群",
      figCaption: "邀请链接",
      howToJoin: "如何加入",
      steps: [
        "点击按钮，打开 Telegram 邀请链接。",
        "在 Telegram 应用或网页端打开，无需搜索群组。",
        "点击「加入群组」即可成为社区成员。",
      ],
    },
  },
});
