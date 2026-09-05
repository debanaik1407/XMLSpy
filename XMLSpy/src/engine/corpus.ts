/**
 * Phase 0 deliverable: sample documents + multi-GB synthetic corpus generator
 * (browser twin of `xmlspy-bench/src/gen.rs`). The generator builds a Blob
 * from one 1 MiB template repeated N times — the Blob references the same
 * ArrayBuffer so building a 2 GiB corpus costs ~1 MiB of JS heap.
 */

export const SAMPLE_ORDERS = `<?xml version="1.0" encoding="UTF-8"?>
<!-- XMLSpy-rs sample: purchase orders (well-formed) -->
<PurchaseOrders xmlns="urn:xmlspy:orders" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance"
                xsi:schemaLocation="urn:xmlspy:orders orders.xsd" generated="2026-03-01T09:15:00Z">
  <PurchaseOrder OrderNumber="99503" OrderDate="2026-02-20" Priority="high">
    <Address Type="Shipping">
      <Name>Ellen Adams</Name>
      <Street>123 Maple Street</Street>
      <City>Mill Valley</City>
      <State>CA</State>
      <Zip>10999</Zip>
      <Country>USA</Country>
    </Address>
    <Address Type="Billing">
      <Name>Tai Yee</Name>
      <Street>8 Oak Avenue</Street>
      <City>Old Town</City>
      <State>PA</State>
      <Zip>95819</Zip>
      <Country>USA</Country>
    </Address>
    <DeliveryNotes>Please leave packages in shed by driveway. &amp; ring twice.</DeliveryNotes>
    <Items>
      <Item PartNumber="872-AA">
        <ProductName>Lawnmower</ProductName>
        <Quantity>1</Quantity>
        <USPrice>148.95</USPrice>
        <Comment>Confirm this is electric</Comment>
      </Item>
      <Item PartNumber="926-AA">
        <ProductName>Baby Monitor</ProductName>
        <Quantity>2</Quantity>
        <USPrice>39.98</USPrice>
        <ShipDate>2026-03-04</ShipDate>
      </Item>
    </Items>
  </PurchaseOrder>
  <PurchaseOrder OrderNumber="99505" OrderDate="2026-02-22" Priority="normal">
    <Address Type="Shipping">
      <Name>Cristian Osorio</Name>
      <Street>456 Main Street</Street>
      <City>Buffalo</City>
      <State>NY</State>
      <Zip>98112</Zip>
      <Country>USA</Country>
    </Address>
    <Address Type="Billing">
      <Name>Cristian Osorio</Name>
      <Street>456 Main Street</Street>
      <City>Buffalo</City>
      <State>NY</State>
      <Zip>98112</Zip>
      <Country>USA</Country>
    </Address>
    <DeliveryNotes><![CDATA[Signature required <front desk>]]></DeliveryNotes>
    <Items>
      <Item PartNumber="456-NM">
        <ProductName>Power Supply</ProductName>
        <Quantity>1</Quantity>
        <USPrice>45.99</USPrice>
      </Item>
    </Items>
  </PurchaseOrder>
  <?processing-instruction audit="true"?>
</PurchaseOrders>
`;

export const SAMPLE_BROKEN = `<?xml version="1.0" encoding="UTF-8"?>
<catalog xmlns="urn:xmlspy:catalog">
  <book id="bk101" id="dup">
    <author>Gambardella, Matthew</author>
    <title>XML Developer's Guide</title>
    <price currency=USD>44.95</price>
    <description>An in-depth look at creating applications with XML & SmartFix.</description>
  </book>
  <book id="bk102">
    <author>Ralls, Kim</author>
    <title>Midnight Rain</titel>
    <price currency="USD">5.95</price>
    <!-- a comment with -- double dashes -->
    <summary>Ends with ]]> in content</summary>
  </book>
  <magazine>
    <title>Unclosed magazine
</catalog>
`;

export const SAMPLE_XSD = `<?xml version="1.0" encoding="UTF-8"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema" targetNamespace="urn:xmlspy:orders"
           xmlns="urn:xmlspy:orders" elementFormDefault="qualified" version="1.1">
  <xs:element name="PurchaseOrders">
    <xs:complexType>
      <xs:sequence>
        <xs:element ref="PurchaseOrder" minOccurs="0" maxOccurs="unbounded"/>
      </xs:sequence>
      <xs:attribute name="generated" type="xs:dateTime"/>
    </xs:complexType>
  </xs:element>
  <xs:element name="PurchaseOrder">
    <xs:complexType>
      <xs:sequence>
        <xs:element name="Address" type="AddressType" minOccurs="2" maxOccurs="2"/>
        <xs:element name="DeliveryNotes" type="xs:string" minOccurs="0"/>
        <xs:element name="Items" type="ItemsType"/>
      </xs:sequence>
      <xs:attribute name="OrderNumber" type="xs:positiveInteger" use="required"/>
      <xs:attribute name="OrderDate" type="xs:date" use="required"/>
      <xs:attribute name="Priority" default="normal">
        <xs:simpleType>
          <xs:restriction base="xs:string">
            <xs:enumeration value="low"/><xs:enumeration value="normal"/><xs:enumeration value="high"/>
          </xs:restriction>
        </xs:simpleType>
      </xs:attribute>
      <xs:assert test="count(Address[@Type='Shipping']) = 1 and count(Address[@Type='Billing']) = 1"/>
    </xs:complexType>
  </xs:element>
  <xs:complexType name="AddressType">
    <xs:sequence>
      <xs:element name="Name" type="xs:string"/>
      <xs:element name="Street" type="xs:string"/>
      <xs:element name="City" type="xs:string"/>
      <xs:element name="State" type="xs:string"/>
      <xs:element name="Zip" type="xs:string"/>
      <xs:element name="Country" type="xs:NMTOKEN" fixed="USA"/>
    </xs:sequence>
    <xs:attribute name="Type" use="required">
      <xs:simpleType>
        <xs:restriction base="xs:string">
          <xs:enumeration value="Shipping"/><xs:enumeration value="Billing"/>
        </xs:restriction>
      </xs:simpleType>
    </xs:attribute>
  </xs:complexType>
  <xs:complexType name="ItemsType">
    <xs:sequence>
      <xs:element name="Item" maxOccurs="unbounded">
        <xs:complexType>
          <xs:sequence>
            <xs:element name="ProductName" type="xs:string"/>
            <xs:element name="Quantity" type="xs:positiveInteger"/>
            <xs:element name="USPrice" type="xs:decimal"/>
            <xs:element name="Comment" type="xs:string" minOccurs="0"/>
            <xs:element name="ShipDate" type="xs:date" minOccurs="0"/>
          </xs:sequence>
          <xs:attribute name="PartNumber" type="xs:string" use="required"/>
        </xs:complexType>
      </xs:element>
    </xs:sequence>
  </xs:complexType>
</xs:schema>
`;

const CITIES = ["Vienna", "Boston", "Mill Valley", "Buffalo", "Old Town", "Zürich", "São Paulo", "Tōkyō", "Berlin", "Austin"];
const PRODUCTS = ["Lawnmower", "Baby Monitor", "Power Supply", "Router", "Keyboard", "Monitor 27in", "USB-C Hub", "Headset", "Desk Lamp", "Webcam"];

function buildTemplate(targetBytes: number): { text: string; recordsPerChunk: number } {
  const enc = new TextEncoder();
  let out = "";
  let i = 0;
  while (enc.encode(out).length < targetBytes) {
    const n = i++;
    out +=
      `  <PurchaseOrder OrderNumber="${100000 + n}" OrderDate="2026-0${1 + (n % 9)}-${String(1 + (n % 28)).padStart(2, "0")}" Priority="${["low", "normal", "high"][n % 3]}">\n` +
      `    <Address Type="Shipping"><Name>Customer ${n}</Name><Street>${n % 999} Main St</Street><City>${CITIES[n % CITIES.length]}</City><State>${["CA", "NY", "TX", "WA"][n % 4]}</State><Zip>${10000 + (n % 89999)}</Zip><Country>USA</Country></Address>\n` +
      `    <Items>\n` +
      `      <Item PartNumber="${n % 900}-AA"><ProductName>${PRODUCTS[n % PRODUCTS.length]}</ProductName><Quantity>${1 + (n % 5)}</Quantity><USPrice>${(19.99 + (n % 400)).toFixed(2)}</USPrice></Item>\n` +
      `      <Item PartNumber="${n % 700}-BB"><ProductName>${PRODUCTS[(n * 7) % PRODUCTS.length]}</ProductName><Quantity>${1 + (n % 3)}</Quantity><USPrice>${(4.5 + (n % 90)).toFixed(2)}</USPrice><Comment>Batch ${n >> 4} &amp; lot ${n & 15}</Comment></Item>\n` +
      `    </Items>\n` +
      `  </PurchaseOrder>\n`;
  }
  return { text: out, recordsPerChunk: i };
}

/** Build a synthetic corpus of ~`mib` MiB. Cheap in memory: one shared 1 MiB template. */
export function generateCorpus(mib: number): { blob: Blob; name: string; records: number } {
  const header = `<?xml version="1.0" encoding="UTF-8"?>\n<!-- synthetic corpus generated by xmlspy-bench (${mib} MiB) -->\n<PurchaseOrders xmlns="urn:xmlspy:orders" generated="${new Date().toISOString()}">\n`;
  const footer = `</PurchaseOrders>\n`;
  const { text, recordsPerChunk } = buildTemplate(1024 * 1024);
  const chunk = new TextEncoder().encode(text);
  const parts: BlobPart[] = [header];
  const count = Math.max(1, Math.round((mib * 1024 * 1024) / chunk.byteLength));
  for (let i = 0; i < count; i++) parts.push(chunk);
  parts.push(footer);
  return { blob: new Blob(parts, { type: "application/xml" }), name: `corpus-${mib >= 1024 ? mib / 1024 + "GiB" : mib + "MiB"}.xml`, records: count * recordsPerChunk };
}
