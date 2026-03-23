import { useState } from "react";
import { Copy, Check } from "lucide-react";
import { toast } from "sonner";
import { authClient } from "@/lib/auth-client";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Card, CardHeader, CardTitle, CardDescription, CardContent } from "@/components/ui/card";

export function SettingsPage() {
  const session = authClient.useSession();
  const orgs = authClient.useListOrganizations();
  const [copied, setCopied] = useState(false);

  const token = session.data?.session?.token ?? "";
  const organizations = orgs.data ?? [];

  function copyToken() {
    navigator.clipboard.writeText(token);
    setCopied(true);
    toast.success("Token copied");
    setTimeout(() => setCopied(false), 2000);
  }

  return (
    <div className="space-y-6 max-w-2xl">
      <h1 className="text-2xl font-bold">Settings</h1>

      <Card>
        <CardHeader>
          <CardTitle className="text-lg">CLI Bearer Token</CardTitle>
          <CardDescription>
            Use this token to authenticate the AO CLI with your sync server.
            Pass it as <code className="text-xs bg-muted px-1 py-0.5 rounded">Authorization: Bearer &lt;token&gt;</code>
          </CardDescription>
        </CardHeader>
        <CardContent>
          <div className="flex gap-2">
            <Input value={token} readOnly className="font-mono text-xs" />
            <Button variant="outline" size="icon" onClick={copyToken}>
              {copied ? <Check className="h-4 w-4" /> : <Copy className="h-4 w-4" />}
            </Button>
          </div>
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <CardTitle className="text-lg">Organizations</CardTitle>
          <CardDescription>Your organizations</CardDescription>
        </CardHeader>
        <CardContent>
          {organizations.length === 0 ? (
            <p className="text-sm text-muted-foreground">No organizations</p>
          ) : (
            <div className="space-y-2">
              {organizations.map((o) => (
                <div key={o.id} className="flex items-center justify-between p-3 border rounded-md">
                  <div>
                    <p className="font-medium">{o.name}</p>
                    <p className="text-xs text-muted-foreground">{o.slug}</p>
                  </div>
                </div>
              ))}
            </div>
          )}
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <CardTitle className="text-lg">Account</CardTitle>
        </CardHeader>
        <CardContent>
          <div className="space-y-2">
            <div className="flex justify-between text-sm">
              <span className="text-muted-foreground">Name</span>
              <span>{session.data?.user?.name}</span>
            </div>
            <div className="flex justify-between text-sm">
              <span className="text-muted-foreground">Email</span>
              <span>{session.data?.user?.email}</span>
            </div>
          </div>
        </CardContent>
      </Card>
    </div>
  );
}
