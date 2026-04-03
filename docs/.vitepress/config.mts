import { defineConfig } from "vitepress";
import { withMermaid } from "vitepress-plugin-mermaid";

export default withMermaid(
  defineConfig({
    title: "Source Cooperative Data Proxy",
    description:
      "Documentation for the Source Cooperative data proxy — a read-only proxy built as a Cloudflare Worker in Rust.",

    head: [
      [
        "link",
        {
          rel: "stylesheet",
          href: "https://fonts.googleapis.com/css2?family=IBM+Plex+Sans:ital,wght@0,400;0,500;0,600;0,700;1,400&display=swap",
        },
      ],
      ["link", { rel: "icon", href: "/logo-light.svg" }],
    ],

    themeConfig: {
      logo: {
        light: "/logo-light.svg",
        dark: "/logo-dark.svg",
      },

      sidebar: [
        {
          text: "Architecture Decisions",
          items: [
            {
              text: "RFC-001: Data Proxy Re-Architecture",
              link: "/adrs/rfc-001",
            },
            {
              text: "ADR-001: S3 API Compatibility & Credentials",
              link: "/adrs/001-s3-credentials",
            },
            {
              text: "ADR-002: Runtime — Cloudflare Workers",
              link: "/adrs/002-runtimes",
            },
            {
              text: "ADR-003: Rust as Implementation Language",
              link: "/adrs/003-rust",
            },
            {
              text: "ADR-004: Inbound Authentication — OIDC & STS",
              link: "/adrs/004-sts",
            },
            {
              text: "ADR-005: Authorization Model",
              link: "/adrs/005-authorization",
            },
            {
              text: "ADR-006: Outbound Connectivity — OIDC & object_store",
              link: "/adrs/006-outbound-storage",
            },
            {
              text: "ADR-007: Configuration Layer",
              link: "/adrs/007-configuration",
            },
          ],
        },
      ],

      editLink: {
        pattern:
          "https://github.com/source-cooperative/data.source.coop/edit/main/docs/:path",
      },

      socialLinks: [
        {
          icon: "github",
          link: "https://github.com/source-cooperative/data.source.coop",
        },
      ],
    },
  })
);
