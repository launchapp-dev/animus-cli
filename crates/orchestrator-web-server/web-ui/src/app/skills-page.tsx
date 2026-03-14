import { useState, useMemo } from "react";
import { useQuery, useMutation } from "@/lib/graphql/client";
import { Card, CardContent } from "@/components/ui/card";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { PageLoading, PageError, SectionHeading } from "./shared";
import { Search, ArrowLeft, ChevronRight } from "lucide-react";
import { toast } from "sonner";

const SKILLS_QUERY = `query Skills { skills { name description category source skillType } }`;
const SKILL_DETAIL_QUERY = `query SkillDetail($name: String!) { skillDetail(name: $name) { name description category source skillType definitionJson } }`;

const CATEGORIES = ["All", "Implementation", "Review", "Research", "Planning", "Testing", "Operations", "Documentation"] as const;

export function SkillsPage() {
  const [result] = useQuery({ query: SKILLS_QUERY });
  const [mode, setMode] = useState<"browse" | "detail">("browse");
  const [selectedSkill, setSelectedSkill] = useState<string | null>(null);
  const [search, setSearch] = useState("");
  const [activeCategory, setActiveCategory] = useState("All");

  if (result.fetching) return <PageLoading />;
  if (result.error) return <PageError message={result.error.message} />;

  const skills: Array<{ name: string; description: string; category: string; source: string; skillType: string }> =
    result.data?.skills ?? [];

  const filtered = useMemo(() => {
    let list = skills;
    if (activeCategory !== "All") {
      list = list.filter((s) => s.category === activeCategory);
    }
    if (search.trim()) {
      const q = search.toLowerCase();
      list = list.filter((s) => s.name.toLowerCase().includes(q) || s.description.toLowerCase().includes(q));
    }
    return list;
  }, [skills, activeCategory, search]);

  const categoryCount = useMemo(() => {
    const counts = new Set(skills.map((s) => s.category));
    return counts.size;
  }, [skills]);

  if (mode === "detail" && selectedSkill) {
    return (
      <SkillDetailView
        name={selectedSkill}
        onBack={() => {
          setMode("browse");
          setSelectedSkill(null);
        }}
      />
    );
  }

  return (
    <div className="space-y-6 ao-fade-in">
      <div className="flex items-center justify-between">
        <div>
          <h1 className="text-2xl font-semibold tracking-tight">Skills</h1>
          <p className="text-xs text-muted-foreground/60 mt-0.5">Discover, install, and manage agent skills</p>
        </div>
      </div>

      <div className="relative">
        <Search className="absolute left-3 top-1/2 -translate-y-1/2 h-3.5 w-3.5 text-muted-foreground/40" />
        <Input
          value={search}
          onChange={(e) => setSearch(e.target.value)}
          placeholder="Search skills..."
          className="pl-9 h-8 text-sm"
        />
      </div>

      <div className="flex gap-1 flex-wrap">
        {CATEGORIES.map((cat) => (
          <button
            key={cat}
            onClick={() => setActiveCategory(cat)}
            className={`px-2.5 py-1 rounded-md text-xs transition-colors ${
              activeCategory === cat
                ? "bg-primary text-primary-foreground"
                : "bg-accent/50 text-muted-foreground hover:text-foreground"
            }`}
          >
            {cat}
          </button>
        ))}
      </div>

      {filtered.length === 0 ? (
        <div className="flex flex-col items-center justify-center py-12 gap-3">
          <p className="text-sm text-muted-foreground/60">No skills match your search</p>
        </div>
      ) : (
        <div className="grid md:grid-cols-2 lg:grid-cols-3 gap-4">
          {filtered.map((skill) => (
            <Card
              key={skill.name}
              className="border-border/40 bg-card/60 hover:border-border/60 transition-colors cursor-pointer group"
              onClick={() => {
                setSelectedSkill(skill.name);
                setMode("detail");
              }}
            >
              <CardContent className="pt-4 pb-3 px-4">
                <div className="flex items-center justify-between mb-2">
                  <span className="text-sm font-mono font-medium">{skill.name}</span>
                  <Badge variant="outline" className="text-[9px] h-4 px-1.5">
                    {skill.source}
                  </Badge>
                </div>
                <p className="text-xs text-muted-foreground/60 line-clamp-2">{skill.description}</p>
                <div className="flex items-center justify-between mt-2">
                  <Badge variant="secondary" className="text-[9px]">
                    {skill.category}
                  </Badge>
                  <ChevronRight className="h-3 w-3 text-muted-foreground/30 opacity-0 group-hover:opacity-100 transition-opacity" />
                </div>
              </CardContent>
            </Card>
          ))}
        </div>
      )}

      <p className="text-[10px] text-muted-foreground/40">
        {skills.length} skills across {categoryCount} categories
      </p>
    </div>
  );
}

function SkillDetailView({ name, onBack }: { name: string; onBack: () => void }) {
  const [result] = useQuery({ query: SKILL_DETAIL_QUERY, variables: { name } });

  if (result.fetching) return <PageLoading />;
  if (result.error) return <PageError message={result.error.message} />;

  const skill = result.data?.skillDetail;
  if (!skill) {
    return (
      <div className="space-y-4">
        <button onClick={onBack} className="text-xs text-muted-foreground hover:text-foreground/70 inline-flex items-center gap-1">
          <ArrowLeft className="h-3 w-3" /> Back to Skills
        </button>
        <p className="text-sm text-muted-foreground">Skill not found</p>
      </div>
    );
  }

  return (
    <div className="space-y-6 ao-fade-in">
      <button onClick={onBack} className="text-xs text-muted-foreground hover:text-foreground/70 inline-flex items-center gap-1">
        <ArrowLeft className="h-3 w-3" /> Back to Skills
      </button>

      <div>
        <div className="flex items-center gap-3">
          <h1 className="text-2xl font-mono font-semibold tracking-tight">{skill.name}</h1>
          <Badge variant="outline">{skill.source}</Badge>
        </div>
        <p className="text-sm text-muted-foreground/70 mt-1">{skill.description}</p>
      </div>

      <Card className="border-border/40 bg-card/60">
        <CardContent className="pt-4 pb-3 px-4">
          <div className="grid grid-cols-3 gap-4 text-xs">
            <div>
              <span className="text-[11px] uppercase tracking-wider text-muted-foreground/60 font-medium">Category</span>
              <p className="mt-0.5">
                <Badge variant="secondary" className="text-[10px]">{skill.category}</Badge>
              </p>
            </div>
            <div>
              <span className="text-[11px] uppercase tracking-wider text-muted-foreground/60 font-medium">Source</span>
              <p className="mt-0.5">{skill.source}</p>
            </div>
            <div>
              <span className="text-[11px] uppercase tracking-wider text-muted-foreground/60 font-medium">Type</span>
              <p className="mt-0.5">{skill.skillType}</p>
            </div>
          </div>
        </CardContent>
      </Card>

      <div>
        <SectionHeading>Definition</SectionHeading>
        <Card className="border-border/40 bg-card/60 mt-2">
          <CardContent className="pt-3 pb-3 px-4">
            <pre className="text-[11px] font-mono text-foreground/70 overflow-x-auto max-h-96 overflow-y-auto whitespace-pre-wrap">
              {skill.definitionJson}
            </pre>
          </CardContent>
        </Card>
      </div>
    </div>
  );
}
