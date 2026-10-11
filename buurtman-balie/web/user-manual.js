import { createLocalization, supportedLanguages } from './localization.js';
import { manualTopics, manualVersion } from './manual-content.js';

// SurrealQuery's host transport contract (src/surreal.rs); keep wire identifiers literal.
const storageTransport = [
  'Before the first load or save, the host must provision bb_workspaces in its selected namespace/database using authorized provisioning credentials. Bind the supplied object as $params, preserving strings, and execute the whole batch in one request. SurrealQuery must return successful statement values in order, including RETURN, only after checking every statement and COMMIT. A RETURN true does not prove COMMIT succeeded. Map BB_STALE_REVISION and confirmed aborted write conflicts to Error::Conflict; unknown transport/commit outcomes remain Error::Storage. The host owns authentication, TLS, timeouts, encoded request/response budgets, bounded workspace provisioning and database durability/backups.',
  'Avant la première lecture ou écriture, l’hôte doit créer bb_workspaces dans son espace de noms et sa base sélectionnés avec des identifiants de provisionnement autorisés. Liez l’objet fourni à $params en conservant les chaînes et exécutez le lot complet en une seule requête. SurrealQuery doit renvoyer les valeurs réussies dans l’ordre, y compris RETURN, après vérification de chaque instruction et de COMMIT. RETURN true ne prouve pas la réussite de COMMIT. Convertissez BB_STALE_REVISION et les conflits d’écriture avec annulation confirmée en Error::Conflict ; les résultats de transport/validation inconnus restent Error::Storage. L’hôte gère authentification, TLS, délais, budgets encodés de requête/réponse, provisionnement borné des espaces et durabilité/sauvegardes.',
  'Vóór de eerste lees- of schrijfactie moet de host bb_workspaces aanmaken in de gekozen namespace/database met bevoegde inrichtingsgegevens. Bind het aangeleverde object als $params, behoud strings en voer de hele batch in één aanvraag uit. SurrealQuery geeft succesvolle instructiewaarden in volgorde terug, inclusief RETURN, pas nadat elke instructie en COMMIT zijn gecontroleerd. RETURN true bewijst niet dat COMMIT is geslaagd. Zet BB_STALE_REVISION en bevestigde afgebroken schrijfconflicten om naar Error::Conflict; onbekende transport/commituitkomsten blijven Error::Storage. De host beheert authenticatie, TLS, time-outs, gecodeerde aanvraag-/antwoordbudgetten, begrensde werkruimte-inrichting en databaseduurzaamheid/back-ups.'
];
const diagnosticsOwnership = [
  'Coordinate shared builds and browser proof with the assigned owner. Follow project build rules and the active gameplan before running checks. Stop or restart only a harness started by your current session; otherwise obtain operator approval for the exact process IDs. Build json_bridge from buurtman-balie-host-demo; the harness defaults to target/debug/examples/json_bridge (.exe on Windows). For alternate target/architecture directories, configure BB_DEMO_BINARY with the actual executable path. Compiling for ARM64 does not prove ARM64 execution. Keep that executable unchanged during browser proof. The host owns production process supervision and recovery.',
  'Coordonnez les compilations partagées et les preuves navigateur avec le responsable désigné. Suivez les règles de compilation du projet et le gameplan actif avant les vérifications. Arrêtez ou redémarrez uniquement un serveur lancé par votre session actuelle ; sinon obtenez l’accord de l’opérateur pour les identifiants de processus exacts. Compilez json_bridge depuis buurtman-balie-host-demo ; le serveur utilise par défaut target/debug/examples/json_bridge (.exe sous Windows). Pour un autre dossier de cible/architecture, configurez BB_DEMO_BINARY avec le chemin réel de l’exécutable. Compiler pour ARM64 ne prouve pas son exécution sur ARM64. Ne modifiez pas cet exécutable pendant la preuve navigateur. L’hôte gère supervision et reprise des processus de production.',
  'Stem gedeelde builds en browserproeven af met de aangewezen eigenaar. Volg de projectbouwregels en het actieve gameplan vóór controles. Stop of herstart alleen een harness die uw huidige sessie heeft gestart; vraag anders operatorgoedkeuring voor de exacte proces-ID’s. Bouw json_bridge vanuit buurtman-balie-host-demo; de harness gebruikt standaard target/debug/examples/json_bridge (.exe op Windows). Stel voor andere doel-/architectuurmappen BB_DEMO_BINARY in op het echte uitvoerbare pad. Compileren voor ARM64 bewijst geen ARM64-uitvoering. Wijzig dat bestand niet tijdens de browserproef. De host beheert productieprocesbewaking en herstel.'
];

/** Offline, host-embeddable internal manual. No credentials, navigation or form mutation. */
export function mountUserManual(root, {localization = createLocalization(), instanceId = 'bb-help'} = {}) {
  if (!/^[A-Za-z][A-Za-z0-9_-]*$/.test(instanceId)) throw new TypeError('Use a stable ASCII manual instance ID.');
  const panel = document.createElement('details');
  panel.className = 'crm-panel'; panel.dataset.testid = 'user-manual';
  const summary = document.createElement('summary');
  summary.style.cursor = 'pointer';
  const title = document.createElement('h2');
  const version = document.createElement('p'); version.textContent = `Buurtman Balie · ${manualVersion}`;
  const label = document.createElement('label');
  const labelText = document.createTextNode(''); label.append(labelText);
  const select = document.createElement('select'); select.dataset.manualLanguage = '';
  for (const [value, text] of [['en','English'],['fr','Français'],['nl','Nederlands']]) select.append(new Option(text,value));
  label.append(select);
  const navigation = document.createElement('nav');
  const article = document.createElement('article');
  const rows = manualTopics.map(topic => {
    const section = document.createElement('section'); section.id = `${instanceId}-${topic.id}`; section.dataset.manualTopic = topic.id;
    const heading = document.createElement('h3');
    const paragraph = document.createElement('p');
    section.append(heading, paragraph);
    const supplement = topic.id === 'storage-swarm' ? storageTransport : topic.id === 'run-diagnostics' ? diagnosticsOwnership : null;
    const transport = supplement ? document.createElement('p') : null;
    if (transport) { if (topic.id === 'storage-swarm') transport.dataset.manualTransport = ''; section.append(transport); }
    if(topic.code) { const pre = document.createElement('pre'), code = document.createElement('code'); code.textContent = topic.code; pre.style.cssText = 'white-space:pre-wrap;overflow-wrap:anywhere;font-size:0.85rem'; pre.append(code); section.append(pre); }
    const link = document.createElement('a'); link.href = `#${section.id}`; link.style.cssText='display:inline-block;margin:0 .75rem .5rem 0;color:var(--crm-accent,#92c5ff)';
    navigation.append(link); article.append(section);
    return {topic, heading, paragraph, link, transport, supplement};
  });
  panel.append(summary, title, version, label, navigation, article); root.append(panel);
  const headings = ['Internal user manual','Manuel utilisateur interne','Interne gebruikershandleiding'];
  const labels = ['Manual language','Langue du manuel','Taal van de handleiding'];
  const contents = ['Manual topics','Rubriques du manuel','Handleidingonderwerpen'];
  function render() {
    const index = supportedLanguages.indexOf(localization.language);
    panel.lang = localization.language; summary.textContent = headings[index]; title.textContent = headings[index];
    labelText.textContent = labels[index]; select.value = localization.language;
    navigation.setAttribute('aria-label', contents[index]);
    for(const row of rows) {
      row.heading.textContent = row.topic.title[index]; row.link.textContent = row.topic.title[index]; row.paragraph.textContent = row.topic.body[index];
      if (row.transport) row.transport.textContent = row.supplement[index];
    }
  }
  const change = () => localization.setLanguage(select.value);
  select.addEventListener('change', change);
  const unsubscribe = localization.subscribe(render); render();
  return {localization, setLanguage: localization.setLanguage,
    dispose() { unsubscribe(); select.removeEventListener('change',change); panel.remove(); }};
}
