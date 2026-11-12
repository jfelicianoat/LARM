Aquí tienes una explicación detallada sobre el funcionamiento de una caché LRU (Least Recently Used).

---

### ¿Qué es una caché LRU?

El acrónimo **LRU** significa **"Least Recently Used"** (Menos Recientemente Utilizado). Es una de las estrategias de gestión de memoria más comunes y eficientes para implementar sistemas de caché. Su principio fundamental es sencillo: **si un dato se ha utilizado recientemente, es muy probable que se vuelva a necesitar en un futuro cercano.**

Por el contrario, si un dato no se ha utilizado en mucho tiempo, es el candidato ideal para ser eliminado cuando el espacio de la caché se agite. Este concepto se basa en el principio de "localidad temporal".

### La estructura de datos detrás del LRU

Para que una caché LRU sea eficiente, debe cumplir dos requisitos técnicos:
1.  **Acceso rápido:** Debe poder encontrar un elemento casi instantáneamente (tiempo constante, $O(1)$).
2.  **Orden dinámico:** Debe poder reordenar los elementos cada vez que uno es accedido o añadido.

Para lograr esto, una caché LRU no utiliza una sola estructura, sino una combinación de dos:
*   **Una Tabla Hash (Hash Map):** Permite encontrar el valor asociado a una clave en tiempo $O(1)$.
*   **Una Lista Doblemente Enlazada:** Mantiene el orden de uso. Cada vez que un elemento se accede, se mueve al "frente" de la lista. El elemento al final de la lista es, por definición, el menos recientemente usado.

### ¿Cómo funciona el proceso?

Cuando el sistema intenta acceder a un dato (operación `get`):
1.  Busca la clave en el Hash Map.
2.  Si existe, el sistema localiza el nodo correspondiente en la lista enlazada y lo mueve al principio.
3.  Si