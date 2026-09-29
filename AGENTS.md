# Cronos AI: fàbrica de programari orientada a agents

Utilitzant Herdr com a entorn d'execució és una de les arquitectures més potents i modernes per a la generació de codi autònoma. Com que Herdr actua com a gestor de terminals locals persistent i invisible, resol el gran problema de les "fàbriques de programari": la pèrdua de context i la dificultat de supervisar múltiples agents treballant en paral·lel.

A continuació, es mostra com estructurar el flux de treball per crear productes complets des de zero mitjançant una arquitectura multiagent.

## Arquitectura de Cronos AI (Model de 4 Capes)

[ Usuari: Idea de Producte ]

- 1. L'ORQUESTRADOR (Python + LangGraph) <── Controla l'estat global (via Socket API / CLI)
- 2. L'ENTORN DE EXECUCIÓ (Herdr Runtime) <── Manté les sessions vives
- Cada panell (Pane 1) (Pane 2) (Pane 3) té un agent especialitzat en una tasca concreta
- 3. PANELL DE CONTROL (Herdr Sidebar / Atenció Queue) <── Monitoratge humà (Working / Blocked / Done)

## Flux de Treball Pas a Pas## Pas 1: Ingesta i Planificació (The Product Manager Agent)

- Acció: L'usuari introdueix un requisit ("Vull una aplicació SaaS de notes amb Markdown i base de dades").
- Tecnologia: Un agent de planificació (creat en Python amb LangGraph) processa el prompt i genera un PRD (Product Requirement Document) dividit en subtasques tècniques
- Utilitza OpenSpec per generar un diagrama de dependències entre les tasques i assigna prioritat a cada agent especialitzat.

## Pas 2: Orquestració i Desplegament de la "Fàbrica" a Herdr

- Acció: L'agent orquestrador utilitza la [Herdr CLI](https://herdr.dev/docs/cli-reference/) o la seva [Socket API](https://herdr.dev/docs/socket-api/) per aixecar automàticament l'entorn de treball.
- Execució en terminal: El script de Python llança comandes com:

```
herdr session create cronos-ai
herdr pane split --right
herdr pane split --bottom
```

- Això crea diferents espais a la terminal, de manera que cada tasca té el seu propi canal d'execució independent.

## Pas 3: Execució Especialitzada (The Builders)

L'orquestrador envia prompts i arrenca agents especialitzats en línia de comandes dins de cada panell de Herdr:

## Workflow de Cronos AI

Llegeig el document ./cronos_ai_workflow.mmd en mermaid per entendre el workflow que es vol

Cada node del workflow està basat en un o més skills especialitzats
