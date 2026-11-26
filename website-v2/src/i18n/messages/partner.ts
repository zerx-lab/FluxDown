import { defineMessages } from "../define";

/**
 * 授权经销商证明页(`/digitalychee/`)文案。
 *
 * 证书正文按惯例中英双语并列:页面以当前语言为主行、另一语言为副行,
 * 所以两支都要完整给出证书条目。
 */
export const partner = defineMessages("partner", {
  en: {
    meta: {
      title: "Authorization Certificate — FluxDown",
      description: "FluxDown authorizes DIGITALYCHEE as its official partner in China.",
    },
    figCaption: "authorization certificate",
    logoAlt: "DIGITALYCHEE",
    title: "Authorization Certificate",
    statement: "FluxDown authorizes DIGITALYCHEE as its official partner in China",
    productsLabel: "Responsible for the sales of the following product(s)",
    channelsLabel: "Authorized sales channels",
    verify: "This page is the only official proof of authorization",
  },
  zh: {
    meta: {
      title: "授权证明 — FluxDown",
      description: "FluxDown 授权「数码荔枝」为大中国区官方合作伙伴。",
    },
    figCaption: "授权证明",
    logoAlt: "数码荔枝",
    title: "授权证明",
    statement: "FluxDown 授权「数码荔枝」为大中国区官方合作伙伴",
    productsLabel: "负责以下软件在中国的销售事宜",
    channelsLabel: "该合作伙伴的销售渠道",
    verify: "本页面为唯一官方授权证明",
  },
});
