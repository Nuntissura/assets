/** Host-scoped language control. Canonical records and identifiers never enter this dictionary. */
const phrases = [
  ['Fields refreshed.','Champs actualisés.','Velden vernieuwd.'],
  ['Refresh fields','Actualiser les champs','Velden vernieuwen'],
  ['Choose a field','Choisissez un champ','Kies een veld'],
  ['Create new field','Créer un nouveau champ','Nieuw veld aanmaken'],
  ['Field name','Nom du champ','Veldnaam'],
  ['Advanced: technical key','Avancé : clé technique','Geavanceerd: technische sleutel'],
  ['Generated key','Clé générée','Gegenereerde sleutel'],
  ['Automatic key','Clé automatique','Automatische sleutel'],
  ['Use a custom key','Utiliser une clé personnalisée','Een eigen sleutel gebruiken'],
  ['Retry pending field','Réessayer le champ en attente','Onbevestigd veld opnieuw proberen'],
  ['Canonical key: {key}','Clé canonique : {key}','Canonieke sleutel: {key}'],
  ['Field rejected: {error}. Correct the draft and try again.','Champ refusé : {error}. Corrigez le brouillon et réessayez.','Veld geweigerd: {error}. Corrigeer het concept en probeer opnieuw.'],
  ['Field created. Catalogue refresh failed: {error}. Refresh before creating another field.','Champ créé. Actualisation du catalogue impossible : {error}. Actualisez avant de créer un autre champ.','Veld aangemaakt. Vernieuwen van de catalogus mislukt: {error}. Vernieuw voordat u een ander veld aanmaakt.'],
  ['This field is already available. Use it in records and filters.','Ce champ est déjà disponible. Utilisez-le dans les fiches et les filtres.','Dit veld is al beschikbaar. Gebruik het in records en filters.'],
  ['Enter a field name.','Saisissez un nom de champ.','Voer een veldnaam in.'],
  ['Field name is too long.','Le nom du champ est trop long.','De veldnaam is te lang.'],
  ['Field key must use letters, numbers, hyphens, underscores, dots or colons.','La clé doit utiliser des lettres, chiffres, tirets, traits de soulignement, points ou deux-points.','De veldsleutel mag letters, cijfers, streepjes, underscores, punten of dubbele punten bevatten.'],
  ['Correct input: {error}','Corrigez la saisie : {error}','Corrigeer de invoer: {error}'],
  ['Save rejected: {error}','Enregistrement refusé : {error}','Opslag geweigerd: {error}'],
  ['Save rejected: search and select the record again to reload your retained draft.','Enregistrement refusé : recherchez et sélectionnez à nouveau la fiche pour recharger votre brouillon conservé.','Opslag geweigerd: zoek en selecteer het record opnieuw om uw bewaarde concept opnieuw te laden.'],
  ['Record reloaded. Review your retained draft before saving.','Fiche rechargée. Vérifiez votre brouillon conservé avant de l’enregistrer.','Record opnieuw geladen. Controleer uw bewaarde concept vóór het opslaan.'],
  ['Save not confirmed: {error}. Retry to reconcile.','Enregistrement non confirmé : {error}. Réessayez pour vérifier.','Opslag niet bevestigd: {error}. Probeer opnieuw ter bevestiging.'],
  ['Previous save confirmed. Save your current changes.','Enregistrement précédent confirmé. Enregistrez vos modifications actuelles.','Vorige opslag bevestigd. Sla uw huidige wijzigingen op.'],
  ['Product saved.','Produit enregistré.','Product opgeslagen.'],
  ['Refresh unavailable: {error}','Actualisation indisponible : {error}','Vernieuwen niet beschikbaar: {error}'],
  ['Price preview changed. Calculate again.','L’aperçu du prix a changé. Recalculez.','Het prijsvoorbeeld is gewijzigd. Bereken opnieuw.'],
  ['Use a nonnegative decimal price.','Utilisez un prix décimal positif ou nul.','Gebruik een niet-negatieve decimale prijs.'],
  ['Price has more decimals than the selected currency scale.','Le prix dépasse le nombre de décimales choisi.','De prijs heeft meer decimalen dan de gekozen valutaschaal.'],
  ['Price exceeds supported range.','Le prix dépasse la plage prise en charge.','De prijs overschrijdt het ondersteunde bereik.'],
  ['Select 0 to 6 price decimal places.','Choisissez entre 0 et 6 décimales pour le prix.','Kies 0 tot 6 prijsdecimalen.'],
  ['Select currency and tax basis for product prices.','Choisissez la devise et la base fiscale des prix.','Kies de valuta en belastingbasis voor productprijzen.'],
  ['Set a custom price key.','Définissez une clé de prix personnalisé.','Stel een aangepaste prijssleutel in.'],
  ['Use a whole decimal integer.','Utilisez un nombre entier décimal.','Gebruik een geheel decimaal getal.'],
  ['Integer is outside the signed 64-bit range.','Le nombre dépasse la plage signée de 64 bits.','Het getal valt buiten het ondertekende 64-bits bereik.'],
  ['Reusable fields','Champs réutilisables','Herbruikbare velden'],
  ['Define a field once. Use it on products and in search filters.','Définissez un champ une fois pour les produits et les filtres de recherche.','Definieer een veld eenmaal voor producten en zoekfilters.'],
  ['Field key','Clé du champ','Veldsleutel'], ['Field label','Libellé du champ','Veldnaam'], ['Field type','Type de champ','Veldtype'],
  ['Text','Texte','Tekst'], ['Integer','Entier','Geheel getal'], ['Boolean','Booléen','Booleaans'],
  ['Available on','Disponible pour','Beschikbaar voor'], ['Product','Produit','Product'], ['Customer','Client','Klant'],
  ['Distributor','Distributeur','Distributeur'], ['Organization','Organisation','Organisatie'], ['Support case','Demande d’assistance','Ondersteuningsvraag'], ['Interaction','Interaction','Interactie'],
  ['Create field','Créer le champ','Veld aanmaken'], ['Select at least one record type.','Sélectionnez au moins un type de fiche.','Selecteer minstens één recordtype.'],
  ['Saving field…','Enregistrement du champ…','Veld opslaan…'], ['Field created. Available for records and filters.','Champ créé. Disponible pour les fiches et les filtres.','Veld aangemaakt. Beschikbaar voor records en filters.'],
  ['A field with this key already exists. Reuse it or choose another key.','Cette clé existe déjà. Réutilisez-la ou choisissez une autre clé.','Deze veldsleutel bestaat al. Gebruik deze of kies een andere sleutel.'],
  ['Field not confirmed: {error}. Retry to reconcile.','Champ non confirmé : {error}. Réessayez pour vérifier.','Veld niet bevestigd: {error}. Probeer opnieuw ter bevestiging.'],
  ['Fields unavailable: {error}','Champs indisponibles : {error}','Velden niet beschikbaar: {error}'],
  ['Clients · B2C / B2B','Clients · B2C / B2B','Klanten · B2C / B2B'], ['Client name','Nom du client','Klantnaam'],
  ['Client status (mandatory)','Statut du client (obligatoire)','Klantstatus (verplicht)'], ['Client status','Statut du client','Klantstatus'],
  ['Choose B2C or B2B','Choisissez B2C ou B2B','Kies B2C of B2B'], ['Pricing tier','Tarif','Prijsgroep'],
  ['Standard · follows B2C/B2B status','Standard · selon le statut B2C/B2B','Standaard · volgt B2C/B2B-status'], ['Standard','Standard','Standaard'],
  ['Save client','Enregistrer le client','Klant opslaan'], ['New client','Nouveau client','Nieuwe klant'], ['Extra pricing tiers','Tarifs supplémentaires','Extra prijsgroepen'],
  ['Tier key','Clé du tarif','Prijsgroepsleutel'], ['Tier name','Nom du tarif','Prijsgroepsnaam'], ['Employee','Employé','Werknemer'],
  ['Base pricing','Tarif de base','Basisprijs'], ['Custom','Personnalisé','Aangepast'], ['Custom base key','Clé de base personnalisée','Aangepaste basissleutel'],
  ['Extra discount (%)','Remise supplémentaire (%)','Extra korting (%)'], ['Create pricing tier','Créer le tarif','Prijsgroep aanmaken'],
  ['Client price preview','Aperçu du prix client','Klantprijsvoorbeeld'], ['Assigned client','Client attribué','Toegewezen klant'], ['Priced product','Produit tarifé','Geprijsd product'],
  ['Calculate client price','Calculer le prix client','Klantprijs berekenen'], ['Choose client','Choisissez un client','Kies een klant'], ['Choose product','Choisissez un produit','Kies een product'],
  ['extra','supplémentaire','extra'], ['base','de base','basis'], ['tax','taxe','belasting'], ['included','incluse','inbegrepen'], ['excluded','exclue','uitgesloten'],
  ['Client saved · {classification} · {tier}','Client enregistré · {classification} · {tier}','Klant opgeslagen · {classification} · {tier}'],
  ['Client save not confirmed: {error}','Enregistrement du client non confirmé : {error}','Klantopslag niet bevestigd: {error}'],
  ['Use a percentage with up to two decimals.','Utilisez un pourcentage avec au maximum deux décimales.','Gebruik een percentage met maximaal twee decimalen.'],
  ['Discount must be between 0 and 100%.','La remise doit être comprise entre 0 et 100 %.','Korting moet tussen 0 en 100% liggen.'],
  ['Pricing tier created.','Tarif créé.','Prijsgroep aangemaakt.'], ['Tier save not confirmed: {error}','Enregistrement du tarif non confirmé : {error}','Prijsgroepopslag niet bevestigd: {error}'],
  ['Calculating…','Calcul en cours…','Berekenen…'], ['Price unavailable: {error}','Prix indisponible : {error}','Prijs niet beschikbaar: {error}'],
  ['Clients unavailable: {error}','Clients indisponibles : {error}','Klanten niet beschikbaar: {error}'],
  ['Conflict','Conflit','Conflict'], ['Storage','Stockage indisponible','Opslag niet beschikbaar'], ['Unauthorized','Non autorisé','Niet toegestaan'],
  ['Invalid','Valeur non valide','Ongeldige waarde'], ['NotFound','Introuvable','Niet gevonden'], ['Capacity','Capacité atteinte','Capaciteit bereikt'],
  ['Denied','Accès refusé','Toegang geweigerd'], ['OperationReused','Identifiant d’opération déjà utilisé','Bewerkings-ID al gebruikt'],
  ['Request failed','Demande non confirmée','Verzoek niet bevestigd'],
];
const dictionary = new Map(phrases.map(row => [row[0], row]));
export const supportedLanguages = Object.freeze(['en', 'fr', 'nl']);
export function createLocalization(initial = 'en') {
  let language;
  const listeners = new Set();
  function setLanguage(value) {
    if (!supportedLanguages.includes(value)) throw new RangeError(`Unsupported CRM language: ${value}`);
    if (language === value) return;
    language = value;
    for (const listener of listeners) listener();
  }
  setLanguage(initial);
  return {
    get language() { return language; }, setLanguage,
    subscribe(listener) { listeners.add(listener); return () => listeners.delete(listener); },
    text(key, values = {}) {
      const phrase = dictionary.get(key)?.[supportedLanguages.indexOf(language)] ?? key;
      return phrase.replace(/\{(\w+)\}/g, (_, name) => String(values[name] ?? ''));
    },
    error(message) {
      const code = String(message).match(/^(Denied|Invalid|NotFound|Conflict|OperationReused|Capacity|Storage)(?:\b|\()/)?.[1];
      return this.text(dictionary.has(message) ? message : code || 'Request failed');
    },
    money(minor, scale) {
      const value = BigInt(minor), absolute = value < 0n ? -value : value, factor = 10n ** BigInt(scale);
      const decimal = new Intl.NumberFormat(language).formatToParts(1.1).find(part => part.type === 'decimal').value;
      return `${value < 0n ? '-' : ''}${absolute / factor}${scale ? decimal + String(absolute % factor).padStart(scale, '0') : ''}`;
    },
  };
}

/** Bind existing text/accessible names in place; input values and DOM identity are preserved. */
export function bindLocalization(root, localization) {
  const bindings = [], walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT);
  while (walker.nextNode()) {
    const node = walker.currentNode, key = node.textContent.trim();
    if (dictionary.has(key)) bindings.push(() => { node.textContent = localization.text(key); });
  }
  for (const node of root.querySelectorAll('[aria-label], [placeholder]')) {
    for (const attribute of ['aria-label', 'placeholder']) {
      const key = node.getAttribute(attribute);
      if (dictionary.has(key)) bindings.push(() => node.setAttribute(attribute, localization.text(key)));
    }
  }
  const render = () => { root.lang = localization.language; for (const binding of bindings) binding(); };
  render(); const unsubscribe = localization.subscribe(render);
  return unsubscribe;
}
