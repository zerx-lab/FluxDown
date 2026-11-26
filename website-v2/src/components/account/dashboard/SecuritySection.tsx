import { LogOut } from "lucide-react";
import type { DashboardCtx } from "./context";
import PasswordForm from "./PasswordForm";
import ProfileForm from "./ProfileForm";
import { ErrorLine, Panel, SectionHead, Spinner, SubHead } from "./ui";
import { useSignOut } from "./useSignOut";

export default function SecuritySection({ ctx }: { ctx: DashboardCtx }) {
  const { t, lang } = ctx;
  const s = t.security;
  const signOut = useSignOut(lang);

  return (
    <section aria-labelledby="section-heading">
      <SectionHead title={s.title} lede={s.lede} />
      <div className="mt-8 space-y-10">
        <ProfileForm ctx={ctx} />
        <PasswordForm ctx={ctx} />
        <div className="space-y-5">
          <SubHead title={s.session.heading} lede={s.session.desc} />
          <Panel className="p-4 sm:p-5">
            <button type="button" onClick={() => void signOut.run()} disabled={signOut.busy} className="btn btn-secondary disabled:opacity-60">
              {signOut.busy ? <Spinner /> : <LogOut size={15} aria-hidden />}
              {signOut.busy ? t.identity.signingOut : t.identity.signOut}
            </button>
            <ErrorLine text={signOut.error} />
          </Panel>
        </div>
      </div>
    </section>
  );
}
